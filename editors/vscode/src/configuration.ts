export interface InspectedSetting<T> {
  readonly defaultValue?: T;
  readonly globalValue?: T;
  readonly workspaceFolderValue?: T;
  readonly workspaceValue?: T;
}

function nonEmpty(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

export function configuredServerPath(
  inspected: InspectedSetting<string> | undefined,
  workspaceTrusted: boolean,
): { readonly path?: string; readonly workspaceValueIgnored: boolean } {
  if (!inspected) return { workspaceValueIgnored: false };
  if (workspaceTrusted) {
    const workspace =
      nonEmpty(inspected.workspaceFolderValue) ?? nonEmpty(inspected.workspaceValue);
    if (workspace) return { path: workspace, workspaceValueIgnored: false };
  }
  const ignored =
    !workspaceTrusted &&
    Boolean(nonEmpty(inspected.workspaceFolderValue) ?? nonEmpty(inspected.workspaceValue));
  return {
    path: nonEmpty(inspected.globalValue) ?? nonEmpty(inspected.defaultValue),
    workspaceValueIgnored: ignored,
  };
}
