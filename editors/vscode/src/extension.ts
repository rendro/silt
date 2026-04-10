import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

export function activate(_context: vscode.ExtensionContext): void {
  const config = vscode.workspace.getConfiguration('silt');
  const serverPath = config.get<string>('serverPath', 'silt');

  const serverOptions: ServerOptions = {
    run: { command: serverPath, args: ['lsp'], transport: TransportKind.stdio },
    debug: { command: serverPath, args: ['lsp'], transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'silt' }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.silt'),
    },
  };

  client = new LanguageClient('silt', 'Silt Language Server', serverOptions, clientOptions);
  client.start().catch((err) => {
    vscode.window.showErrorMessage(
      `Failed to start silt LSP (${serverPath} lsp): ${err instanceof Error ? err.message : String(err)}`,
    );
  });
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) {
    return undefined;
  }
  return client.stop();
}
