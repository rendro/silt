Content-Length: 933

{"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": "file:///fuzz/main.silt", "languageId": "silt", "version": 1, "text": "import list\n\ntype Shape {\n  Circle(Float),\n  Rect(Float, Float),\n}\n\ntrait Display for Shape {\n  fn display(self) -> String {\n    match self {\n      Circle(r) -> \"Circle(r={r})\"\n      Rect(w, h) -> \"Rect({w}x{h})\"\n    }\n  }\n}\n\nfn area(shape) {\n  match shape {\n    Circle(r) -> 3.14159 * r * r\n    Rect(w, h) -> w * h\n  }\n}\n\nfn main() {\n  let shapes = [Circle(5.0), Rect(3.0, 4.0), Circle(1.0)]\n  println(\"-- Shapes --\")\n  shapes |> list.each { s -> println(\"{s} -> area = {area(s)}\") }\n  let total = shapes |> list.map { s -> area(s) } |> list.fold(0.0) { acc, a -> acc + a }\n  println(\"\\nTotal area: {total}\")\n  let big = shapes |> list.filter { s -> area(s) > 10.0 }\n  println(\"Shapes with area > 10: {list.length(big)}\")\n}\n"}}}Content-Length: 931

{"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {"textDocument": {"uri": "file:///fuzz/main.silt", "version": 2}, "contentChanges": [{"text": "import list\n\ntype Shape {\n  Circle(Float),\n  Rect(Float, Float),\n}\n\ntrait Display for Shape {\n  fn display(self) -> String {\n    match self {\n      Circle(r) -> \"Circle(r={r})\"\n      Rect(w, h) -> \"Rect({w}x{h})\"\n    }\n  }\n}\n\nfn area(shape) {\n  match shape {\n    Circle(r) -> 3.0 * r * r\n    Rect(w, h) -> w * h\n  }\n}\n\nfn main() {\n  let shapes = [Circle(5.0), Rect(3.0, 4.0), Circle(1.0)]\n  println(\"-- Shapes --\")\n  shapes |> list.each { s -> println(\"{s} -> area = {area(s)}\") }\n  let total = shapes |> list.map { s -> area(s) } |> list.fold(0.0) { acc, a -> acc + a }\n  println(\"\\nTotal area: {total}\")\n  let big = shapes |> list.filter { s -> area(s) > 10.0 }\n  println(\"Shapes with area > 10: {list.length(big)}\")\n}\n"}]}}Content-Length: 131

{"jsonrpc": "2.0", "id": 1, "method": "textDocument/documentSymbol", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}}}Content-Length: 176

{"jsonrpc": "2.0", "id": 2, "method": "textDocument/formatting", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "options": {"tabSize": 2, "insertSpaces": true}}}Content-Length: 136

{"jsonrpc": "2.0", "id": 3, "method": "textDocument/semanticTokens/full", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}}}Content-Length: 129

{"jsonrpc": "2.0", "id": 4, "method": "textDocument/foldingRange", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}}}Content-Length: 212

{"jsonrpc": "2.0", "id": 5, "method": "textDocument/inlayHint", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 30, "character": 0}}}}Content-Length: 127

{"jsonrpc": "2.0", "id": 6, "method": "textDocument/diagnostic", "params": {"textDocument": {"uri": "file:///fuzz/main.silt"}}}