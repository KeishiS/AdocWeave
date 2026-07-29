import { constants } from "node:fs";
import { access, stat } from "node:fs/promises";
import { delimiter, extname, isAbsolute, resolve } from "node:path";

function isLoaderEnvironmentVariable(name) {
  return (
    name.startsWith("DYLD_") ||
    name.startsWith("LD_") ||
    name === "GLIBC_TUNABLES" ||
    name === "NIX_LD" ||
    name.startsWith("NIX_LD_")
  );
}

export function hostExecutableEnvironment(environment) {
  return Object.fromEntries(
    Object.entries(environment).filter(([name]) => !isLoaderEnvironmentVariable(name)),
  );
}

export async function resolveHostExecutable(command, environment = process.env) {
  if (isAbsolute(command)) {
    await requireExecutableFile(command);
    return command;
  }
  if (command.includes("/") || command.includes("\\")) {
    const absolute = resolve(command);
    await requireExecutableFile(absolute);
    return absolute;
  }
  const extensions =
    process.platform === "win32" && extname(command) === ""
      ? (environment.PATHEXT ?? ".COM;.EXE;.BAT;.CMD").split(";")
      : [""];
  for (const directory of (environment.PATH ?? "").split(delimiter)) {
    if (directory === "") continue;
    for (const extension of extensions) {
      const candidate = resolve(directory, `${command}${extension}`);
      try {
        await requireExecutableFile(candidate);
        return candidate;
      } catch {
        // 次の候補を確認します。
      }
    }
  }
  throw new Error(`host executable not found: ${command}`);
}

async function requireExecutableFile(path) {
  await access(path, constants.X_OK);
  if (!(await stat(path)).isFile()) throw new Error(`host executable is not a file: ${path}`);
}
