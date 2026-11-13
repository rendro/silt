//! The server on messages in memory, without stdio: what the check of
//! the editor's messages runs (`fuzz_invariants::check_lsp_messages`,
//! the fuzz target `fuzz_lsp_messages`).

use lsp_server::{Connection, Message, RequestId};

use super::Server;

/// The server, as after the start, on the editor's messages `messages`,
/// sent faster than the analysis waits: each is handled in turn, a
/// request after the analysis of the edits before it, and the analysis
/// of the last edits at the end; a shutdown ends it. What the server
/// sent, and the ids of the requests it handled, in order.
pub(crate) fn serve(messages: Vec<Message>) -> (Vec<Message>, Vec<RequestId>) {
    let (connection, editor) = Connection::memory();
    for message in messages {
        // (The server's receiver lives until it is dropped below.)
        let _ = editor.sender.send(message);
    }
    // With every message queued and the editor's sender gone, the
    // server's receive ends after the last one: so does a shutdown's
    // wait for the `exit` notification.
    drop(editor.sender);
    let mut server = Server::new(connection);
    let mut handled = Vec::new();
    while let Ok(message) = server.connection.receiver.recv() {
        if let Message::Request(request) = &message {
            handled.push(request.id.clone());
        }
        if server.handle_message(message) {
            break;
        }
    }
    // A request runs the analysis that is due before it is answered; what
    // the last edits scheduled runs here, as once the editor pauses.
    if server.deadline.is_some() {
        server.analyse_pending();
    }
    drop(server);
    (editor.receiver.try_iter().collect(), handled)
}
