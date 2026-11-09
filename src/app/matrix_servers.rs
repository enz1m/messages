use super::AccountForm;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Server {
    #[default]
    MatrixOrg,
    Mozilla,
    Tchncs,
    Custom,
}

impl Server {
    pub const ALL: [Self; 4] = [Self::MatrixOrg, Self::Mozilla, Self::Tchncs, Self::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Self::MatrixOrg => "Matrix.org (default)",
            Self::Mozilla => "Mozilla (mozilla.org)",
            Self::Tchncs => "tchncs.de",
            Self::Custom => "Custom server…",
        }
    }

    pub fn endpoint(self) -> Option<&'static str> {
        match self {
            Self::MatrixOrg => Some("https://matrix.org"),
            Self::Mozilla => Some("https://mozilla.modular.im"),
            Self::Tchncs => Some("https://matrix.tchncs.de"),
            Self::Custom => None,
        }
    }

    pub fn for_endpoint(endpoint: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|server| {
                server.endpoint().is_some_and(|url| {
                    endpoint
                        .trim()
                        .trim_end_matches('/')
                        .eq_ignore_ascii_case(url)
                })
            })
            .unwrap_or(Self::Custom)
    }
}

impl AccountForm {
    pub(super) fn identify_matrix_server(&mut self) {
        self.matrix_server = Server::for_endpoint(&self.endpoint);
        if self.matrix_server == Server::Custom {
            self.matrix_custom_endpoint.clone_from(&self.endpoint);
        }
    }

    pub(super) fn select_matrix_server(&mut self, server: Server) {
        if self.matrix_server == Server::Custom {
            self.matrix_custom_endpoint.clone_from(&self.endpoint);
        }
        self.matrix_server = server;
        self.endpoint = server
            .endpoint()
            .map(str::to_owned)
            .unwrap_or_else(|| self.matrix_custom_endpoint.clone());
    }
}
