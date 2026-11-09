use super::*;
use enzui_iced_widgets::row_measure::RowMeasurement;

#[tokio::test]
async fn large_chats_realize_bounded_rows_preserve_each_anchor_and_place_stale_measurements_by_key()
{
    let directory = tempfile::tempdir().unwrap();
    let mut app = app_in(directory.path());
    app.window_size = iced::Size::new(1024.0, 768.0);
    let template = app.store.conversations[0].clone();
    let message = template.messages[0].clone();
    app.store.conversations.clear();
    for chat in 0..3 {
        let mut room = template.clone();
        room.key.room = format!("large-{chat}");
        room.draft = format!("Draft {chat}");
        room.messages = (0..10000)
            .map(|i| {
                let mut m = message.clone();
                m.id = format!("m-{i}");
                m.body = format!("Chat {chat}, message {i}");
                m
            })
            .collect();
        app.store.conversations.push(room);
    }
    let keys: Vec<_> = app
        .store
        .conversations
        .iter()
        .map(|r| r.key.clone())
        .collect();
    for key in &keys {
        let task = app.update(Message::Select(key.clone()));
        drive(&mut app, task).await;
        let p = &app.scroll_positions[key];
        assert!(p.layout.follow);
        assert!(p.y > 600000.0);
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let element = crate::view::transcript::counted_view(
            &app,
            app.store.room(key).unwrap(),
            count.clone(),
        );
        assert!(
            (5..40).contains(&count.get()),
            "realized rows: {}",
            count.get()
        );
        drop(element);
        let p = app.scroll_positions.get_mut(key).unwrap();
        p.layout.follow = false;
        p.y = 7200.0;
    }
    let key = keys.last().unwrap();
    let revision = app.revision;
    let draft = app.current_room().unwrap().draft.clone();
    for action in [
        text_editor::Action::Move(text_editor::Motion::Left),
        text_editor::Action::Select(text_editor::Motion::Right),
        text_editor::Action::Scroll { lines: 1 },
    ] {
        let task = app.update(Message::Editor(action));
        assert!(outputs(task).await.is_empty());
        assert_eq!(app.revision, revision);
        assert_eq!(app.current_room().unwrap().draft, draft);
    }
    let _ = app.update(Message::WorkChanged);
    let _ = app.update(Message::ComposerMeasured(key.clone(), 128.0));
    let _ = app.update(Message::TranscriptViewport(
        key.clone(),
        iced::Size::new(700.0, 600.0),
    ));
    paste(&mut app, "An actual edit");
    assert!(app.revision > revision);
    assert_ne!(app.current_room().unwrap().draft, draft);
    let first = keys[0].clone();
    let task = app.update(Message::Select(first.clone()));
    drive(&mut app, task).await;
    assert!((app.scroll_positions[&first].y - 7200.0).abs() < 1.0);
    assert_eq!(app.store.room(&first).unwrap().draft, "Draft 0");
    let old_generation = app.scroll_positions[&first].layout.generation;
    let anchor_index = app.scroll_positions[&first]
        .layout
        .model
        .index_at(7200.0)
        .unwrap();
    let anchor = app.scroll_positions[&first].layout.keys[anchor_index].clone();
    let measured_before_prepend = app.scroll_positions[&first].layout.keys[1].clone();
    let mut older = message.clone();
    older.id = "prepended".into();
    app.store
        .room_mut(&first)
        .unwrap()
        .messages
        .insert(0, older);
    let _ = app.prepare_transcript(false);
    let p = &app.scroll_positions[&first];
    assert_eq!(p.layout.keys[p.layout.model.index_at(p.y).unwrap()], anchor);
    // A report rendered before the prepend still reaches its own row (now one
    // further down), never the row that took its old index, and the anchor
    // row stays where the reader is.
    let _ = app.measure_transcript(
        first.clone(),
        old_generation,
        &measured_before_prepend,
        RowMeasurement {
            index: 1,
            height: 900.0,
        },
    );
    let p = &app.scroll_positions[&first];
    let moved = p
        .layout
        .keys
        .iter()
        .position(|k| *k == measured_before_prepend)
        .unwrap();
    assert_eq!(moved, 2);
    assert_eq!(p.layout.model.height_of(moved), 900.0);
    assert_ne!(p.layout.model.height_of(1), 900.0);
    assert_eq!(p.layout.keys[p.layout.model.index_at(p.y).unwrap()], anchor);
    let generation = app.scroll_positions[&first].layout.generation;
    let prepended = app.scroll_positions[&first].layout.keys[1].clone();
    let _ = app.measure_transcript(
        first.clone(),
        generation,
        &prepended,
        RowMeasurement {
            index: 1,
            height: 200.0,
        },
    );
    let p = &app.scroll_positions[&first];
    assert_eq!(p.layout.keys[p.layout.model.index_at(p.y).unwrap()], anchor);
    let measured = app.scroll_positions[&first].layout.model.height_of(1);
    app.window_size.width = 400.0;
    let _ = app.prepare_transcript(false);
    assert_ne!(app.scroll_positions[&first].layout.generation, generation);
    assert_eq!(
        app.scroll_positions[&first].layout.model.height_of(1),
        measured
    );
    let _ = app.jump_to_bottom(&first);
    assert!(app.scroll_positions[&first].layout.follow);

    let room = app.store.room_mut(&first).unwrap();
    for message in &mut room.messages {
        message.content_state = Some(messages_core::MessageContentState::WaitingForKeys);
        message.body = "Hidden pending payload".into();
    }
    let length = room.messages.len();
    room.messages[length - 2].content_state = Some(messages_core::MessageContentState::Available);
    room.messages[length - 2].body = "A readable new message".into();
    room.messages[length - 1].content_state = Some(messages_core::MessageContentState::Available);
    room.messages[length - 1].body = "Waiting for encryption keys…".into();
    assert!(room.has_locked_history());
    assert_eq!(room.preview(), "Waiting for encryption keys…");
    room.messages[0].content_state = Some(messages_core::MessageContentState::Relation);
    room.messages[0].deleted = true;
    let anchor = room.messages[length - 2].id.clone();
    let _ = app.prepare_transcript(false);
    assert_eq!(app.scroll_positions[&first].layout.keys.len(), 4);
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let element =
        crate::view::transcript::counted_view(&app, app.store.room(&first).unwrap(), count.clone());
    assert_eq!(count.get(), 4);
    drop(element);
    app.search = "Hidden pending".into();
    assert!(app.search_hits().is_empty());
    assert!(
        app.store
            .visible_rooms("Hidden pending", None, false, false)
            .is_empty()
    );
    app.search = "Waiting for encryption keys".into();
    assert_eq!(app.search_hits().len(), 1);
    app.search.clear();
    let p = app.scroll_positions.get_mut(&first).unwrap();
    p.layout.follow = false;
    p.viewport_height = 100.0;
    p.y = p.layout.model.offset_of(1);
    let recovered = &mut app.store.room_mut(&first).unwrap().messages[1];
    recovered.content_state = Some(messages_core::MessageContentState::Available);
    recovered.body = "Restored older message".into();
    let _ = app.prepare_transcript(false);
    let p = &app.scroll_positions[&first];
    assert_eq!(
        p.layout.keys[p.layout.model.index_at(p.y).unwrap()],
        format!("m:{anchor}")
    );
    assert_eq!(app.store.room(&first).unwrap().draft, "Draft 0");
    for message in &mut app.store.room_mut(&first).unwrap().messages {
        if message.waiting_for_keys() {
            message.content_state = Some(messages_core::MessageContentState::Available);
        }
    }
    assert!(!app.store.room(&first).unwrap().has_locked_history());
    assert!(!app.store.room(&first).unwrap().messages[0].is_readable());
    let mut delayed = app.store.room(&first).unwrap().messages[0].clone();
    delayed.content_state = Some(messages_core::MessageContentState::WaitingForKeys);
    delayed.deleted = false;
    app.store.merge_messages(&first, vec![delayed]).unwrap();
    assert!(!app.store.room(&first).unwrap().messages[0].is_readable());
}
