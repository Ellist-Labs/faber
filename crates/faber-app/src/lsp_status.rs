use faber_lsp::manager::ServerStatus;

pub struct LspStatus {
    pub statuses: Vec<ServerStatus>,
}

impl LspStatus {
    pub fn new() -> Self {
        Self { statuses: vec![] }
    }
}
