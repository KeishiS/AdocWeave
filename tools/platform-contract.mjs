import path from "node:path";

export const WINDOWS_DLL_ALLOWLIST = new Set([
  "advapi32.dll",
  "bcrypt.dll",
  "bcryptprimitives.dll",
  "crypt32.dll",
  "iphlpapi.dll",
  "kernel32.dll",
  "normaliz.dll",
  "ntdll.dll",
  "ole32.dll",
  "secur32.dll",
  "shell32.dll",
  "user32.dll",
  "userenv.dll",
  "ws2_32.dll",
]);

export const TEMPORARY_DIRECTORY_REMOVAL_OPTIONS = Object.freeze({
  recursive: true,
  force: true,
  maxRetries: 10,
  retryDelay: 100,
});

export function createRuntimeAdapters({ fileSystem, processControl, time, platform, pathApi }) {
  for (const [name, value] of Object.entries({ fileSystem, processControl, time, platform, pathApi })) {
    if (!value || typeof value !== "object") throw new TypeError(`${name} adapter is required`);
  }
  if (typeof platform.os !== "string" || typeof platform.architecture !== "string") {
    throw new TypeError("platform adapter requires os and architecture");
  }
  return Object.freeze({ fileSystem, processControl, time, platform, pathApi });
}

export function pathImplementation(os) {
  return os === "win32" ? path.win32 : path.posix;
}

export function executableNames(executableSuffix) {
  return [`adocweave${executableSuffix}`, `adocweave-lsp${executableSuffix}`];
}

export function selectedInstallationFamilies(scope = "complete") {
  if (!["complete", "native-only", "global-only"].includes(scope)) {
    throw new Error("installation scope must be complete, native-only, or global-only");
  }
  return Object.freeze({
    native: scope !== "global-only",
    global: scope !== "native-only",
  });
}

export function requiredInstallationAssets(scope, target, version, archiveType) {
  const families = selectedInstallationFamilies(scope);
  return [
    ...(families.native
      ? [
        `adocweave-cli-${target}.${archiveType}`,
        `adocweave-lsp-${target}.${archiveType}`,
      ]
      : []),
    ...(families.global
      ? [
        `adocweave-browser-${version}.tar.xz`,
        `adocweave-textlint-plugin-asciidoc-${version}.tgz`,
        `adocweave-zed-${version}.tar.xz`,
        `adocweave-vscode-${version}.vsix`,
      ]
      : []),
  ];
}

export function missingInstallationAssets(available, required) {
  const names = new Set(available);
  return required.filter((name) => !names.has(name));
}

export function installationLayout(prefix, version, pathApi) {
  const productRoot = pathApi.join(prefix, "lib", "adocweave");
  const shareRoot = pathApi.join(prefix, "share", "adocweave", version);
  return {
    binDirectory: pathApi.join(prefix, "bin"),
    versionRoot: pathApi.join(productRoot, version),
    currentLink: pathApi.join(productRoot, "current"),
    activeMarker: pathApi.join(productRoot, "active-version"),
    browserRoot: pathApi.join(shareRoot, "browser"),
    zedRoot: pathApi.join(shareRoot, "zed"),
    vscodeRoot: pathApi.join(shareRoot, "vscode"),
  };
}

export function isPathInside(root, candidate, pathApi) {
  const relative = pathApi.relative(root, candidate);
  return relative === "" || (!relative.startsWith(`..${pathApi.sep}`) && relative !== ".." &&
    !pathApi.isAbsolute(relative));
}

export function archiveEntries(listing) {
  return listing.replaceAll("\r\n", "\n").split("\n").filter(Boolean);
}

export function validateArchiveEntries(entries, expectedRoot) {
  return entries.filter((entry) =>
    entry.startsWith("/") ||
    entry.includes("\\") ||
    entry.split("/").includes("..") ||
    (expectedRoot && entry !== `${expectedRoot}/` && !entry.startsWith(`${expectedRoot}/`)));
}

export function importedWindowsDlls(dumpbinOutput) {
  return [...dumpbinOutput.matchAll(/^\s+([A-Za-z0-9_.-]+\.dll)\s*$/gim)]
    .map((match) => match[1].toLowerCase());
}

export function unexpectedWindowsDlls(imported) {
  return imported.filter((name) =>
    !WINDOWS_DLL_ALLOWLIST.has(name) &&
    !name.startsWith("api-ms-win-") &&
    !name.startsWith("ext-ms-win-"));
}

export function unexpectedMacosDependencies(otoolOutput) {
  return otoolOutput.split(/\r?\n/).slice(1).map((line) => line.trim()).filter((line) =>
    line && !line.startsWith("/usr/lib/") && !line.startsWith("/System/Library/"));
}

export function macosMinimumVersion(otoolOutput) {
  return /cmd LC_BUILD_VERSION[\s\S]*?minos ([0-9.]+)/.exec(otoolOutput)?.[1];
}

export function shouldRetryRemoval(error, platform) {
  return platform === "win32" && error instanceof Error &&
    ["EBUSY", "EMFILE", "ENFILE", "ENOTEMPTY", "EPERM"].includes(error.code);
}

export function vscodePackageContract(manifest, version) {
  return manifest.version === version && manifest.main === "./dist/extension.cjs";
}
