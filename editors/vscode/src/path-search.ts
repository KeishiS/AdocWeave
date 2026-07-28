import { constants } from "node:fs";
import { access } from "node:fs/promises";
import { isAbsolute, join } from "node:path";

function executableNames(name: string, os: NodeJS.Platform): readonly string[] {
  if (os !== "win32") return [name];
  return [`${name}.exe`];
}

export async function findOnPath(
  name: string,
  pathValue: string | undefined = process.env.PATH,
  os: NodeJS.Platform = process.platform,
): Promise<string | undefined> {
  if (!pathValue || isAbsolute(name)) return undefined;
  const pathDelimiter = os === "win32" ? ";" : ":";
  for (const directory of pathValue.split(pathDelimiter).filter(Boolean)) {
    for (const executable of executableNames(name, os)) {
      const candidate = join(directory, executable);
      try {
        await access(candidate, os === "win32" ? constants.F_OK : constants.X_OK);
        return candidate;
      } catch {
        // 次の候補を確認します。
      }
    }
  }
  return undefined;
}
