use std::path::{Path, PathBuf};

use messages_core::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, OutgoingAttachment};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

static FILE_JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub fn safe_name(name: &str) -> String {
    let mut cleaned = String::new();
    for character in name
        .chars()
        .filter(|character| !character.is_control() && !['/', '\\', ':'].contains(character))
    {
        if cleaned.len() + character.len_utf8() > 180 {
            break;
        }
        cleaned.push(character);
    }
    let name = cleaned;
    let name = name.trim().trim_matches('.');
    if name.is_empty() {
        "Attachment".into()
    } else {
        name.into()
    }
}

async fn private_directory(path: &Path) -> Result<(), String> {
    let absolute =
        std::path::absolute(path).map_err(|_| "Could not resolve private file storage.")?;
    let path = absolute.as_path();
    let mut missing = Vec::new();
    let mut ancestor = path;
    while !ancestor.exists() {
        missing.push(ancestor.to_owned());
        ancestor = ancestor.parent().ok_or("File storage has no parent.")?;
    }
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(path)
        .await
        .map_err(|_| "Could not create private file storage.")?;
    for created in &missing {
        sync_directory(created).await?;
        if let Some(parent) = created.parent() {
            sync_directory(parent).await?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| "Could not protect private file storage.")?;
    }
    Ok(())
}

async fn sync_directory(path: &Path) -> Result<(), String> {
    tokio::fs::File::open(path)
        .await
        .map_err(|_| "Could not open file storage for synchronization.")?
        .sync_all()
        .await
        .map_err(|_| "Could not synchronize file storage.".into())
}

pub async fn stage(
    root: PathBuf,
    paths: Vec<PathBuf>,
    available: usize,
    budget: u64,
) -> Result<Vec<OutgoingAttachment>, String> {
    let _permit = FILE_JOBS
        .acquire()
        .await
        .map_err(|_| "File processing stopped.")?;
    if paths.is_empty() || paths.len() > available.min(MAX_ATTACHMENTS) {
        return Err("Attach at most 10 files at a time.".into());
    }
    private_directory(&root).await?;
    let batch = root.join(Uuid::new_v4().to_string());
    private_directory(&batch).await?;
    let result = stage_batch(&batch, paths, budget.min(MAX_ATTACHMENT_BYTES)).await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(batch).await;
    }
    result
}

async fn stage_batch(
    root: &Path,
    paths: Vec<PathBuf>,
    mut budget: u64,
) -> Result<Vec<OutgoingAttachment>, String> {
    let mut staged = Vec::new();
    for path in paths {
        let file = tokio::fs::File::open(&path)
            .await
            .map_err(|_| "Could not open an attached file.")?;
        let metadata = file
            .metadata()
            .await
            .map_err(|_| "Could not read an attached file.")?;
        if !metadata.is_file() {
            return Err("Choose files rather than folders.".into());
        }
        if metadata.len() > budget {
            return Err("Attachments can total up to 100 MB per message.".into());
        }
        let id = Uuid::new_v4();
        let directory = root.join(id.to_string());
        private_directory(&directory).await?;
        let name = safe_name(&path.file_name().unwrap_or_default().to_string_lossy());
        let destination = directory.join(&name);
        let mut output = tokio::fs::OpenOptions::new();
        output.write(true).create_new(true);
        #[cfg(unix)]
        output.mode(0o600);
        let mut output = output
            .open(&destination)
            .await
            .map_err(|_| "Could not store an attached file.")?;
        let bytes = tokio::io::copy(&mut file.take(budget + 1), &mut output)
            .await
            .map_err(|_| "Could not store an attached file.")?;
        if bytes > budget {
            return Err("Attachments can total up to 100 MB per message.".into());
        }
        output
            .sync_all()
            .await
            .map_err(|_| "Could not save an attached file.")?;
        // The snapshot must not outlive the directory entry naming its attachment.
        sync_directory(&directory).await?;
        budget -= bytes;
        staged.push(OutgoingAttachment {
            id,
            path: destination,
            name,
            bytes,
            mime: mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string(),
        });
    }
    Ok(staged)
}

pub async fn discard(root: PathBuf, files: Vec<OutgoingAttachment>) {
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return;
    };
    for file in files {
        if let Some(directory) = file.path.parent()
            && let Ok(directory) = tokio::fs::canonicalize(directory).await
            && directory != root
            && directory.starts_with(&root)
        {
            let _ = tokio::fs::remove_dir_all(directory).await;
        }
    }
}

pub async fn publish(destination: PathBuf, bytes: Vec<u8>) -> Result<PathBuf, String> {
    let _permit = FILE_JOBS
        .acquire()
        .await
        .map_err(|_| "File processing stopped.")?;
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err("This file exceeds the 100 MB limit.".into());
    }
    let parent = destination
        .parent()
        .ok_or("Choose a folder to save into.")?;
    let temporary = parent.join(format!(".messages-{}.part", Uuid::new_v4()));
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut output = options
            .open(&temporary)
            .await
            .map_err(|_| "Could not create the saved file.")?;
        output
            .write_all(&bytes)
            .await
            .map_err(|_| "Could not write the saved file.")?;
        output
            .sync_all()
            .await
            .map_err(|_| "Could not save the file.")?;
        drop(output);
        // A link publishes a complete file and refuses to replace an existing destination.
        tokio::fs::hard_link(&temporary, &destination)
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "That file already exists. Choose another name."
                } else {
                    "Could not publish the saved file."
                }
            })?;
        sync_directory(parent).await?;
        Ok(destination.clone())
    }
    .await;
    let _ = tokio::fs::remove_file(temporary).await;
    result
}

/// The global budget for copies of attachments opened in other apps, across
/// every account: total bytes, file count and age.
pub const OPENED_CACHE_BYTES: u64 = 300 * 1024 * 1024;
pub const OPENED_CACHE_FILES: usize = 128;
pub const OPENED_CACHE_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Publish an attachment copy for opening under `files/<account>/opened`,
/// then hold every account's opened copies to the global budget.
pub async fn cache(
    files_root: PathBuf,
    account: Uuid,
    name: String,
    bytes: Vec<u8>,
) -> Result<PathBuf, String> {
    let root = files_root.join(account.to_string()).join("opened");
    private_directory(&root).await?;
    let folder = root.join(Uuid::new_v4().to_string());
    private_directory(&folder).await?;
    let path = publish(folder.join(safe_name(&name)), bytes).await?;
    prune_opened(files_root, &path).await?;
    Ok(path)
}

pub async fn read_private(root: PathBuf, path: &str) -> Result<Vec<u8>, String> {
    let _permit = FILE_JOBS
        .acquire()
        .await
        .map_err(|_| "File processing stopped.")?;
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| "The local attachment is unavailable.")?;
    let path = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| "The local attachment is unavailable.")?;
    if !path.starts_with(&root) {
        return Err("The attachment is outside this account's private storage.".into());
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| "The local attachment is unavailable.")?;
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "Could not read the attachment.")?;
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err("This file exceeds the 100 MB limit.".into());
    }
    Ok(bytes)
}

/// Evict opened-attachment copies, oldest first, until every account's
/// `opened` cache together fits [`OPENED_CACHE_BYTES`] and
/// [`OPENED_CACHE_FILES`], and drop copies older than [`OPENED_CACHE_AGE`].
///
/// Only `files/<account>/opened/<copy>/` folders are candidates: staged
/// outgoing attachments (`outgoing`, still in send custody) and files the
/// user saved elsewhere are never visited. Symbolic links are not followed,
/// `keep` (the copy just handed to another app) is never evicted, and the
/// scan is bounded.
async fn prune_opened(files_root: PathBuf, keep: &Path) -> Result<(), String> {
    const MAX_ACCOUNTS: usize = 256;
    const MAX_FOLDERS: usize = 4096;
    let _permit = FILE_JOBS
        .acquire()
        .await
        .map_err(|_| "File processing stopped.")?;
    let failed = |_| "Could not maintain the attachment cache.";
    let mut accounts = tokio::fs::read_dir(&files_root).await.map_err(failed)?;
    let mut copies = Vec::new();
    let mut folders = 0;
    let mut visited = 0;
    while visited < MAX_ACCOUNTS && folders < MAX_FOLDERS {
        let Some(account) = accounts.next_entry().await.map_err(failed)? else {
            break;
        };
        visited += 1;
        if !account.file_type().await.map_err(failed)?.is_dir() {
            continue;
        }
        let opened = account.path().join("opened");
        let mut entries = match tokio::fs::read_dir(&opened).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failed(error).into()),
        };
        while folders < MAX_FOLDERS {
            let Some(folder) = entries.next_entry().await.map_err(failed)? else {
                break;
            };
            if !folder.file_type().await.map_err(failed)?.is_dir() {
                continue;
            }
            folders += 1;
            let mut children = tokio::fs::read_dir(folder.path()).await.map_err(failed)?;
            while let Some(file) = children.next_entry().await.map_err(failed)? {
                // `DirEntry::metadata` does not traverse symbolic links.
                let metadata = file.metadata().await.map_err(failed)?;
                if metadata.is_file() {
                    copies.push((
                        file.path(),
                        metadata.len(),
                        metadata
                            .modified()
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    ));
                }
            }
        }
    }
    for path in evictions(copies, keep) {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::remove_dir_all(parent).await;
        }
    }
    Ok(())
}

/// The copies to evict from `copies` (path, bytes, modified), oldest first.
fn evictions(mut copies: Vec<(PathBuf, u64, std::time::SystemTime)>, keep: &Path) -> Vec<PathBuf> {
    copies.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    let mut size: u64 = copies.iter().map(|(_, bytes, _)| bytes).sum();
    let mut count = copies.len();
    let mut evicted = Vec::new();
    for (file, bytes, modified) in copies {
        if file == keep {
            continue;
        }
        if size > OPENED_CACHE_BYTES
            || count > OPENED_CACHE_FILES
            || modified.elapsed().is_ok_and(|age| age > OPENED_CACHE_AGE)
        {
            evicted.push(file);
            size = size.saturating_sub(bytes);
            count = count.saturating_sub(1);
        }
    }
    evicted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn copy(files_root: &Path, account: Uuid, name: &str, age: Duration) -> PathBuf {
        let folder = files_root
            .join(account.to_string())
            .join("opened")
            .join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(name);
        std::fs::write(&path, b"copy").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
        path
    }

    fn opened_count(files_root: &Path) -> usize {
        std::fs::read_dir(files_root)
            .unwrap()
            .filter_map(|account| std::fs::read_dir(account.ok()?.path().join("opened")).ok())
            .map(Iterator::count)
            .sum()
    }

    /// agent_c/enz-messages#3: opened copies share one budget across every
    /// account; outgoing custody and links out of the cache are never touched.
    #[tokio::test]
    async fn opened_copies_enforce_private_global_age_and_byte_budgets() {
        {
            let directory = tempfile::tempdir().unwrap();
            let files_root = directory.path().join("files");
            let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
            // The oldest copy belongs to the other account: a per-account budget
            // would never evict it.
            let oldest = copy(&files_root, second, "oldest.pdf", Duration::from_secs(3600));
            for index in 1..OPENED_CACHE_FILES {
                let account = if index % 2 == 0 { first } else { second };
                copy(&files_root, account, "doc.pdf", Duration::from_secs(60));
            }
            let staged = files_root
                .join(first.to_string())
                .join("outgoing/batch/item");
            std::fs::create_dir_all(&staged).unwrap();
            std::fs::write(staged.join("photo.jpg"), b"custody").unwrap();
            // An old file staged a long time ago stays: custody is not a cache.
            std::fs::File::options()
                .write(true)
                .open(staged.join("photo.jpg"))
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(30 * 24 * 3600))
                .unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("exported.pdf"), b"saved by the user").unwrap();
            std::os::unix::fs::symlink(
                outside.path(),
                files_root.join(second.to_string()).join("opened/link"),
            )
            .unwrap();

            let opened = cache(files_root.clone(), first, "new.pdf".into(), b"new".to_vec())
                .await
                .unwrap();

            assert!(opened.exists(), "the copy just opened is kept");
            assert!(!oldest.exists(), "the globally oldest copy is evicted");
            // 128 real copies plus the link, which is never counted or followed.
            assert_eq!(opened_count(&files_root), OPENED_CACHE_FILES + 1);
            assert_eq!(std::fs::read(staged.join("photo.jpg")).unwrap(), b"custody");
            assert!(outside.path().join("exported.pdf").exists());
        }

        {
            let directory = tempfile::tempdir().unwrap();
            let files_root = directory.path().join("files");
            let account = Uuid::new_v4();
            let stale = copy(
                &files_root,
                account,
                "stale.pdf",
                OPENED_CACHE_AGE + Duration::from_secs(60),
            );
            let fresh = copy(&files_root, account, "fresh.pdf", Duration::from_secs(60));
            cache(
                files_root.clone(),
                account,
                "new.pdf".into(),
                b"new".to_vec(),
            )
            .await
            .unwrap();
            assert!(!stale.exists() && fresh.exists());

            let now = SystemTime::now();
            let big = OPENED_CACHE_BYTES / 2 + 1;
            let keep = PathBuf::from("/c/keep");
            let evicted = evictions(
                vec![
                    (keep.clone(), 1, now - Duration::from_secs(300)),
                    (PathBuf::from("/a/old"), big, now - Duration::from_secs(200)),
                    (PathBuf::from("/b/new"), big, now - Duration::from_secs(100)),
                ],
                &keep,
            );
            assert_eq!(evicted, [PathBuf::from("/a/old")]);
        }
    }
}
