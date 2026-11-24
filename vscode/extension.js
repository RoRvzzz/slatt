const vscode = require('vscode');
const { checkRules } = require('./check');

const opcodes = [
  'LOADK', 'MOVE', 'ADD', 'SUB', 'MUL', 'DIV', 'XOR32', 'JUMP', 'BR_TRUE',
  'BR_EQ', 'BR_LT', 'BR_LE', 'VARARG', 'RETURN', 'NATIVE', 'NATIVE_BRANCH', 'NATIVE_RETURN',
];

function activate(context) {
  const diagnostics = vscode.languages.createDiagnosticCollection('slatt');
  const pending = new Map();

  function cancel(key) {
    const task = pending.get(key);
    if (!task) return;
    clearTimeout(task.timer);
    task.controller.abort();
    pending.delete(key);
  }

  function schedule(document) {
    const key = document.uri.toString();
    cancel(key);
    if (document.languageId !== 'slatt' || !vscode.workspace.isTrusted) {
      diagnostics.delete(document.uri);
      return;
    }

    const controller = new AbortController();
    const timer = setTimeout(async () => {
      const executable = vscode.workspace.getConfiguration('slatt').get('executablePath') || 'slatt';
      const sourcePath = document.uri.scheme === 'file' ? document.uri.fsPath : undefined;
      const finding = await checkRules(executable, document.getText(), controller.signal, sourcePath);
      if (controller.signal.aborted || document.isClosed) return;
      pending.delete(key);
      if (!finding) {
        diagnostics.delete(document.uri);
        return;
      }
      const range = new vscode.Range(finding.line, finding.start, finding.line, finding.end);
      const severity = finding.toolError ? vscode.DiagnosticSeverity.Information : vscode.DiagnosticSeverity.Error;
      const diagnostic = new vscode.Diagnostic(range, finding.message, severity);
      diagnostic.source = 'slatt';
      diagnostics.set(document.uri, [diagnostic]);
    }, 250);
    pending.set(key, { timer, controller });
  }

  const provider = {
    provideCompletionItems(document) {
      const keywords = document.languageId === 'slatt'
        ? ['include', 'frontend', 'pass', 'map', 'match', 'where', 'and', 'fold', 'constants', 'thread', 'jumps', 'prune', 'unreachable', 'dce']
        : ['proto', 'raw', 'using', 'params', 'vararg'];
      const items = keywords.map((word) => new vscode.CompletionItem(word, vscode.CompletionItemKind.Keyword));
      for (const name of [...opcodes, 'const', 'known', 'bits']) {
        const item = new vscode.CompletionItem(name, vscode.CompletionItemKind.Function);
        item.insertText = new vscode.SnippetString(`${name}($0)`);
        items.push(item);
      }
      for (const name of ['nil', 'true', 'false']) items.push(new vscode.CompletionItem(name, vscode.CompletionItemKind.Constant));
      return items;
    },
  };

  context.subscriptions.push(
    diagnostics,
    { dispose: () => [...pending.keys()].forEach(cancel) },
    vscode.languages.registerCompletionItemProvider(['slatt', 'slatt-ir'], provider),
    vscode.workspace.onDidOpenTextDocument(schedule),
    vscode.workspace.onDidChangeTextDocument(({ document }) => schedule(document)),
    vscode.workspace.onDidSaveTextDocument(document => {
      if (document.languageId === 'slatt') vscode.workspace.textDocuments.forEach(schedule);
    }),
    vscode.workspace.onDidCloseTextDocument(({ uri }) => {
      cancel(uri.toString());
      diagnostics.delete(uri);
    }),
    vscode.workspace.onDidGrantWorkspaceTrust(() => vscode.workspace.textDocuments.forEach(schedule)),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration('slatt.executablePath')) vscode.workspace.textDocuments.forEach(schedule);
    }),
  );
  vscode.workspace.textDocuments.forEach(schedule);
}

module.exports = { activate };
