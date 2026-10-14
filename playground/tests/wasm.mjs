// The playground as a page runs it: the wasm module, one thread, the
// page's clock. Run after `wasm-pack build --target web`:
//
//   node tests/wasm.mjs [pkg]
//
// Each program gets a module of its own, as a page gives one to each
// run that follows a trap.

import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const pkg = resolve(process.argv[2] ?? 'pkg');
const glue = pathToFileURL(resolve(pkg, 'silt_playground.js')).href;
const bytes = await readFile(resolve(pkg, 'silt_playground_bg.wasm'));

let loads = 0;
async function load() {
  const mod = await import(`${glue}?${loads++}`);
  await mod.default({ module_or_path: bytes });
  return mod;
}

let failed = 0;
async function test(name, body) {
  try {
    await body(await load());
    console.log(`ok    ${name}`);
  } catch (e) {
    failed++;
    console.log(`FAIL  ${name}\n${String(e.stack ?? e).replace(/^/gm, '      ')}`);
  }
}

/// Run `source`; a trap fails the test with the panic's message.
function run(mod, source) {
  try {
    return JSON.parse(mod.run(source));
  } catch (e) {
    throw new Error(`the run trapped: ${mod.panic_message() || e}`);
  }
}

await test('the version is silt\'s', (mod) => {
  assert.match(mod.version(), /^\d+\.\d+\.\d+$/);
});

await test('a program runs', (mod) => {
  const ran = run(mod, 'import list\nfn main() {\n  1..3 |> list.each { n -> println(n) }\n}\n');
  assert.deepEqual(ran, { diagnostics: '', output: '1\n2\n3\n', error: '', ok: true });
});

await test('tasks run on the thread that waits for them', (mod) => {
  const ran = run(mod, `import channel
import list
import task

fn main() {
  let ch = channel.new(10)
  let w1 = task.spawn { -> channel.each(ch) { msg -> println("w1: {msg}") } }
  let w2 = task.spawn { -> channel.each(ch) { msg -> println("w2: {msg}") } }
  list.each(1..6) { n -> channel.send(ch, n) }
  channel.close(ch)
  task.join(w1)
  task.join(w2)
}
`);
  assert.equal(ran.ok, true, ran.error);
  const lines = ran.output.trim().split('\n');
  assert.deepEqual(lines.map((line) => line.slice(4)).sort(), ['1', '2', '3', '4', '5', '6']);
});

await test('a sleep outside a task moves the clock on and does not wait', (mod) => {
  const started = Date.now();
  const ran = run(mod, `import time

fn main() {
  let before = time.now()
  time.sleep(time.hours(1))
  println(time.since(before, time.now()))
}
`);
  assert.equal(ran.output, '1h\n', ran.error);
  assert.ok(Date.now() - started < 30_000);
});

await test('a sleep in a task ends: the clock lets the time pass', (mod) => {
  const ran = run(mod, `import task
import time

fn main() {
  let sleeper = task.spawn { ->
    time.sleep(time.hours(1))
    println("slept")
  }
  task.join(sleeper)
  println("joined")
}
`);
  assert.deepEqual([ran.output, ran.error], ['slept\njoined\n', '']);
});

await test('a wait with a timeout ends at its deadline', (mod) => {
  const ran = run(mod, `import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  println(channel.receive_timeout(ch, time.ms(500)))
  println(task.deadline(time.ms(50)) { -> time.sleep(time.hours(1)) })
  println(task.deadline(time.ms(50)) { -> 7 })
}
`);
  assert.deepEqual(
    [ran.output, ran.error],
    ['Empty\nErr(the function was ended by its deadline)\nOk(7)\n', ''],
  );
});

await test('a task that still sleeps when main returns is waited for', (mod) => {
  const ran = run(mod, `import task
import time

fn main() {
  let unjoined = task.spawn { ->
    time.sleep(time.minutes(5))
    println("late")
  }
  println("main is done")
}
`);
  assert.deepEqual([ran.output, ran.error], ['main is done\nlate\n', '']);
});

await test('a deadlock is reported', (mod) => {
  const ran = run(mod, 'import channel\nfn main() {\n  let ch = channel.new(0)\n  println(channel.receive(ch))\n}\n');
  assert.equal(ran.ok, false);
  assert.match(ran.error, /^error\[runtime\]: deadlock on main thread: channel receive with no counterparty\n --> main\.silt:4:11/);
});

await test('a program that loops for ever is ended', (mod) => {
  const ran = run(mod, 'fn spin(n) {\n  spin(n + 1)\n}\n\nfn main() {\n  println("start")\n  spin(0)\n}\n');
  assert.equal(ran.ok, false);
  assert.equal(ran.output, 'start\n');
  assert.match(ran.error, /the step budget is used up/);
  assert.match(ran.error, /note: the playground ends a program after \d+ steps/);
});

await test('deep recursion is a runtime error, not a trap', (mod) => {
  const ran = run(mod, `fn depth(n) {
  match n {
    0 -> 0
    _ -> 1 + depth(n - 1)
  }
}

fn main() {
  println(depth(1000000))
}
`);
  assert.match(ran.error, /^error\[runtime\]: stack overflow/);
});

await test('the page gives the time and the randomness', (mod) => {
  const ran = run(mod, `import math
import time
import uuid

fn main() {
  println(math.random() < 1.0)
  println(uuid.v4() != uuid.v4())
  println(time.today())
}
`);
  const today = new Date().toISOString().slice(0, 10);
  assert.deepEqual([ran.output, ran.error], [`true\ntrue\n${today}\n`, '']);
});

await test('what a page cannot do is an Err, not a trap', (mod) => {
  const ran = run(mod, 'import io\nfn main() {\n  println(io.read_file("notes.txt"))\n}\n');
  assert.equal(ran.ok, true, ran.error);
  assert.match(ran.output, /^Err\(/);
});

await test('a task that failed and that nobody joined is reported', (mod) => {
  const ran = run(mod, 'import task\nfn main() {\n  let unjoined = task.spawn { -> panic("in the task") }\n  println("main is done")\n}\n');
  assert.match(ran.output, /^main is done\nerror\[runtime\]: task .* failed and was never joined: panic: in the task/);
});

await test('check, format, complete and tokens answer', (mod) => {
  const source = 'fn main() {\n  nowhere\n}\n';
  const problems = JSON.parse(mod.check(source));
  assert.equal(problems.length, 1);
  assert.deepEqual([problems[0].from, problems[0].to, problems[0].severity], [14, 21, 'error']);

  assert.deepEqual(JSON.parse(mod.format('fn main(){println( 1 )}')), { Ok: 'fn main() {\n  println(1)\n}\n' });
  assert.match(JSON.parse(mod.format('fn main( {')).Err, /^error\[parse\]: /);

  const names = JSON.parse(mod.complete(source));
  assert.ok(names.some((item) => item.label === 'list.map' && item.kind === 'function'));
  assert.ok(names.some((item) => item.label === 'main' && item.kind === 'function'));

  const kinds = JSON.parse(mod.token_kinds());
  const tokens = Array.from(mod.tokens('fn main() { 1 } -- é'));
  const texts = [];
  for (let i = 0; i < tokens.length; i += 3) {
    texts.push(['fn main() { 1 } -- é'.slice(tokens[i], tokens[i + 1]), kinds[tokens[i + 2]]]);
  }
  assert.deepEqual(texts, [
    ['fn', 'keyword'],
    ['(', 'punctuation'],
    [')', 'punctuation'],
    ['{', 'punctuation'],
    ['1', 'number'],
    ['}', 'punctuation'],
    ['-- é', 'comment'],
  ]);
});

console.log(failed === 0 ? '\nall passed' : `\n${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
