const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { checkRules, parseDiagnostic } = require('./check');
const pack = require('./package.json');
const grammar = require('./syntaxes/slatt.tmLanguage.json');

test('parser locations map Unicode columns to VS Code UTF-16 positions', () => {
  assert.deepEqual(parseDiagnostic('slatt: -:2:3: expected a token\r\n', '// comment\r\n🚀 x'), {
    line: 1, start: 3, end: 4, message: 'expected a token',
  });
  assert.equal(parseDiagnostic('shell failed', ''), undefined);
});

test('editor manifest references packaged files and highlights all IR opcodes', () => {
  for (const item of pack.contributes.grammars.concat(pack.contributes.snippets)) {
    assert.ok(fs.existsSync(path.join(__dirname, item.path)), item.path);
  }
  const source = fs.readFileSync(path.join(__dirname, '../src/ir.rs'), 'utf8');
  const pattern = new RegExp(grammar.patterns.find((rule) => rule.name === 'support.function.opcode.slatt').match);
  const opcodes = [...source.matchAll(/^\s*"([A-Z][A-Z0-9_]*)" => Self::/gm)].map((match) => match[1]);
  assert.ok(opcodes.length > 10);
  for (const opcode of opcodes) assert.ok(pattern.test(`${opcode}(`), opcode);
});

test('compiler checks unsaved source with no temporary files', { skip: !process.env.SLATT_TEST_EXE }, async () => {
  const exe = process.env.SLATT_TEST_EXE;
  assert.equal(await checkRules(exe, 'pass ok { dce; }'), undefined);
  const bad = await checkRules(exe, 'pass invalid {\n  match [MOVE($a, $b)] => [MOVE($a, $missing)];\n}');
  assert.equal(bad.line, 1);
  assert.match(bad.message, /unknown binding \$missing/);
  const missing = await checkRules(path.join(__dirname, 'missing-slatt.exe'), '');
  assert.equal(missing.toolError, true);
  assert.match(missing.message, /could not be found/);
});
