Content-Length: 933

{"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": "file:///fuzz/main.silt", "languageId": "silt", "version": 1, "text": "import list\n\ntype Shape {\n  Circle(Float),\n  Rect(Float, Float),\n}\n\ntrait Display for Shape {\n  fn display(self) -> String {\n    match self {\n      Circle(r) -> \"Circle(r={r})\"\n      Rect(w, h) -> \"Rect({w}x{h})\"\n    }\n  }\n}\n\nfn area(shape) {\n  match shape {\n    Circle(r) -> 3.14159 * r * r\n    Rect(w, h) -> w * h\n  }\n}\n\nfn main() {\n  let shapes = [Circle(5.0), Rect(3.0, 4.0), Circle(1.0)]\n  println(\"-- Shapes --\")\n  shapes |> list.each { s -> println(\"{s} -> area = {area(s)}\") }\n  let total = shapes |> list.map { s -> area(s) } |> list.fold(0.0) { acc, a -> acc + a }\n  println(\"\\nTotal area: {total}\")\n  let big = shapes |> list.filter { s -> area(s) > 10.0 }\n  println(\"Shapes with area > 10: {list.length(big)}\")\n}\n"}}}Content-Length: 172

{"jsonrpc": "2.0", "id": "a", "method": "textDocument/definition", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 25, "character": 40}}}Content-Length: 212

{"jsonrpc": "2.0", "id": "b", "method": "textDocument/references", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 16, "character": 4}, "context": {"includeDeclaration": true}}}Content-Length: 174

{"jsonrpc": "2.0", "id": "c", "method": "textDocument/prepareRename", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 16, "character": 4}}}Content-Length: 186

{"jsonrpc": "2.0", "id": "d", "method": "textDocument/rename", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 16, "character": 4}, "newName": "size"}}Content-Length: 178

{"jsonrpc": "2.0", "id": "e", "method": "textDocument/documentHighlight", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 16, "character": 4}}}Content-Length: 175

{"jsonrpc": "2.0", "id": "f", "method": "textDocument/typeDefinition", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 24, "character": 7}}}Content-Length: 174

{"jsonrpc": "2.0", "id": "g", "method": "textDocument/implementation", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "position": {"line": 7, "character": 6}}}Content-Length: 87

{"jsonrpc": "2.0", "id": "h", "method": "workspace/symbol", "params": {"query": "are"}}Content-Length: 178

{"jsonrpc": "2.0", "id": "i", "method": "textDocument/selectionRange", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "positions": [{"line": 17, "character": 9}]}}Content-Length: 247

{"jsonrpc": "2.0", "id": "j", "method": "textDocument/codeAction", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 30, "character": 0}}, "context": {"diagnostics": []}}}