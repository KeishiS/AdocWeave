import assert from "node:assert/strict";

import * as vscode from "vscode";

async function waitFor<T>(read: () => T | undefined, timeout = 20_000): Promise<T> {
  const started = Date.now();
  while (Date.now() - started < timeout) {
    const value = read();
    if (value !== undefined) return value;
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
  }
  throw new Error("VS Code extension hostの応答を待機中にtimeoutしました");
}

function hasDiagnostic(uri: vscode.Uri, code: string): boolean {
  return vscode.languages.getDiagnostics(uri).some((diagnostic) => diagnostic.code === code);
}

export async function run(): Promise<void> {
  const folders = vscode.workspace.workspaceFolders;
  assert.equal(folders?.length, 2, "複数workspace folderが初期化されていません");
  assert.ok(folders);
  const rootFolder = folders[0];
  assert.ok(rootFolder);
  const uri = vscode.Uri.file(vscode.Uri.joinPath(rootFolder.uri, "root.adoc").fsPath);
  const document = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(document);

  await waitFor(() => {
    const diagnostics = vscode.languages.getDiagnostics(uri);
    return diagnostics.length > 0 ? diagnostics : undefined;
  });

  const partUri = vscode.Uri.joinPath(rootFolder.uri, "part.adoc");
  const part = await vscode.workspace.openTextDocument(partUri);
  const partEditor = await vscode.window.showTextDocument(part);
  await waitFor(() => (hasDiagnostic(partUri, "heading-marker-space") ? true : undefined));
  await partEditor.edit((builder) => builder.insert(new vscode.Position(0, 2), " "));
  await waitFor(() => (!hasDiagnostic(partUri, "heading-marker-space") ? true : undefined));

  const unicodeUri = vscode.Uri.joinPath(rootFolder.uri, "unicode.adoc");
  await vscode.workspace.openTextDocument(unicodeUri);
  const unicodeDiagnostic = await waitFor(() =>
    vscode.languages
      .getDiagnostics(unicodeUri)
      .find((diagnostic) => diagnostic.code === "trailing-whitespace"),
  );
  assert.deepEqual(unicodeDiagnostic.range, new vscode.Range(0, 5, 0, 6));
  await vscode.window.showTextDocument(document);

  const symbols = await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
    "vscode.executeDocumentSymbolProvider",
    uri,
  );
  assert.ok(symbols.length > 0, "Outlineが空です");

  const hover = await vscode.commands.executeCommand<vscode.Hover[]>(
    "vscode.executeHoverProvider",
    uri,
    new vscode.Position(6, 2),
  );
  assert.ok(hover.length > 0, "Hoverが空です");

  const completion = await vscode.commands.executeCommand<vscode.CompletionList>(
    "vscode.executeCompletionItemProvider",
    uri,
    new vscode.Position(6, 3),
  );
  assert.ok(
    completion.items.some((item) => item.label === "name"),
    "補完に文書属性がありません",
  );

  const definitions = await vscode.commands.executeCommand<vscode.Location[]>(
    "vscode.executeDefinitionProvider",
    uri,
    new vscode.Position(6, 2),
  );
  assert.ok(definitions.length > 0, "Definitionが空です");

  const references = await vscode.commands.executeCommand<vscode.Location[]>(
    "vscode.executeReferenceProvider",
    uri,
    new vscode.Position(1, 2),
  );
  assert.ok(references.length > 1, "Referencesが文書属性の参照を返しません");

  const links = await vscode.commands.executeCommand<vscode.DocumentLink[]>(
    "vscode.executeLinkProvider",
    uri,
  );
  assert.ok(links.length > 0, "Document Linkが空です");

  const edits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
    "vscode.executeFormatDocumentProvider",
    uri,
    { insertSpaces: true, tabSize: 2 },
  );
  assert.ok(edits.length > 0, "Formatが編集を返しません");

  const actions = await vscode.commands.executeCommand<(vscode.CodeAction | vscode.Command)[]>(
    "vscode.executeCodeActionProvider",
    uri,
    new vscode.Range(10, 0, 10, 8),
  );
  assert.ok(actions.length > 0, "Code Actionが空です");

  const rename = await vscode.commands.executeCommand<vscode.WorkspaceEdit>(
    "vscode.executeDocumentRenameProvider",
    uri,
    new vscode.Position(3, 3),
    "renamed",
  );
  assert.ok(rename.size > 0, "Renameが編集を返しません");

  const legend = await vscode.commands.executeCommand<vscode.SemanticTokensLegend>(
    "vscode.provideDocumentSemanticTokensLegend",
    uri,
  );
  const tokens = await vscode.commands.executeCommand<vscode.SemanticTokens>(
    "vscode.provideDocumentSemanticTokens",
    uri,
  );
  assert.ok(legend.tokenTypes.length > 0, "Semantic Tokens legendが空です");
  assert.ok(tokens.data.length > 0, "Semantic Tokensが空です");

  const originalVersion = document.version;
  const editor = vscode.window.activeTextEditor;
  assert.ok(editor);
  await editor.edit((builder) => builder.insert(new vscode.Position(7, 0), "😀日本語 "));
  await waitFor(() => (document.version > originalVersion ? document.version : undefined));

  await editor.edit((builder) => {
    builder.insert(document.lineAt(document.lineCount - 1).range.end, "\n古い結果  ");
  });
  for (let index = 0; index < 8; index += 1) {
    await editor.edit((builder) => {
      const line = document.lineAt(document.lineCount - 1);
      builder.replace(line.range, index === 7 ? "最終結果" : `古い結果 ${index}  `);
    });
  }
  await waitFor(() =>
    vscode.languages
      .getDiagnostics(uri)
      .every((diagnostic) => diagnostic.range.start.line !== document.lineCount - 1)
      ? true
      : undefined,
  );

  const previousDiagnostics = vscode.languages.getDiagnostics(uri);
  await vscode.commands.executeCommand("adocweave.restartServer");
  await editor.edit((builder) =>
    builder.delete(new vscode.Range(new vscode.Position(0, 1), new vscode.Position(0, 2))),
  );
  await waitFor(() => {
    const diagnostics = vscode.languages.getDiagnostics(uri);
    return diagnostics.some((diagnostic) => diagnostic.code === "heading-marker-space") &&
      diagnostics !== previousDiagnostics
      ? diagnostics
      : undefined;
  });

  await vscode.workspace
    .getConfiguration("adocweave")
    .update(
      "server.path",
      vscode.Uri.joinPath(rootFolder.uri, "missing-adocweave-lsp").fsPath,
      vscode.ConfigurationTarget.Global,
    );
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 500));
  await editor.edit((builder) => builder.insert(new vscode.Position(0, 1), " "));
  await waitFor(() => (!hasDiagnostic(uri, "heading-marker-space") ? true : undefined));

  assert.equal(vscode.workspace.updateWorkspaceFolders(1, 1), true);
  await waitFor(() => (vscode.workspace.workspaceFolders?.length === 1 ? true : undefined));
}
