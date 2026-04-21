//! `workspace/symbol` handler (Ctrl+T / @ in VSCode).
#![allow(deprecated)] // SymbolInformation.deprecated field is LSP-required

use lsp_types::{SymbolInformation, WorkspaceSymbolParams, WorkspaceSymbolResponse};

use super::Server;

impl Server {
    pub(super) fn workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Option<WorkspaceSymbolResponse> {
        let symbols: Vec<SymbolInformation> =
            self.workspace_symbols_matching(&params.query);
        if symbols.is_empty() {
            None
        } else {
            Some(WorkspaceSymbolResponse::Flat(symbols))
        }
    }
}
