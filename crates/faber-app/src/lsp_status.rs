use faber_lsp::manager::ServerStatus;

pub struct LspStatus {
    pub statuses: Vec<ServerStatus>,
    pub error_count: usize,
    pub warning_count: usize,
}

impl LspStatus {
    pub fn new() -> Self {
        Self {
            statuses: vec![],
            error_count: 0,
            warning_count: 0,
        }
    }
}
