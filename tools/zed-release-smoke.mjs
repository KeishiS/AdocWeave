import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import process from "node:process";

const [archive] = process.argv.slice(2);
if (!archive) throw new Error("usage: node tools/zed-release-smoke.mjs ARCHIVE");

const version = JSON.parse(readFileSync(new URL("../release-manifest.json", import.meta.url), "utf8")).packageVersion;
const packageName = `adocweave-zed-${version}`;
if (basename(archive) !== `${packageName}.tar.xz`) throw new Error("unexpected Zed archive name");
const expected = [
  `${packageName}/`,
  `${packageName}/Cargo.lock`,
  `${packageName}/Cargo.toml`,
  `${packageName}/LICENSE-APACHE`,
  `${packageName}/LICENSE-MIT`,
  `${packageName}/README.adoc`,
  `${packageName}/THIRD_PARTY_NOTICES.adoc`,
  `${packageName}/extension.toml`,
  `${packageName}/languages/`,
  `${packageName}/languages/asciidoc/`,
  `${packageName}/languages/asciidoc/config.toml`,
  `${packageName}/languages/asciidoc/highlights.scm`,
  `${packageName}/languages/asciidoc/injections.scm`,
  `${packageName}/languages/asciidoc_inline/`,
  `${packageName}/languages/asciidoc_inline/config.toml`,
  `${packageName}/languages/asciidoc_inline/highlights.scm`,
  `${packageName}/languages/asciidoc_inline/injections.scm`,
  `${packageName}/src/`,
  `${packageName}/src/install.rs`,
  `${packageName}/src/lib.rs`,
].sort();
const actual = execFileSync("tar", ["-tJf", archive], { encoding: "utf8" }).trim().split("\n").sort();
if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error("unexpected Zed archive layout");
if (actual.some((entry) => entry.startsWith("/") || entry.split("/").includes(".."))) {
  throw new Error("unsafe path in Zed archive");
}

const root = mkdtempSync(join(tmpdir(), "adocweave-zed-release-"));
try {
  execFileSync("tar", ["-xJf", archive, "-C", root]);
  const extension = readFileSync(join(root, packageName, "extension.toml"), "utf8");
  const cargo = readFileSync(join(root, packageName, "Cargo.toml"), "utf8");
  if (!extension.includes(`version = "${version}"`) || !cargo.includes(`version = "${version}"`)) {
    throw new Error("Zed archive version mismatch");
  }
  if (!extension.includes("[grammars.asciidoc_inline]")) {
    throw new Error("Zed archive is missing the inline grammar declaration");
  }
  for (const grammar of ["asciidoc", "asciidoc_inline"]) {
    for (const query of ["highlights.scm", "injections.scm"]) {
      const source = readFileSync(join(root, packageName, "languages", grammar, query), "utf8");
      if (!source.trim()) throw new Error(`empty Zed query: ${grammar}/${query}`);
    }
  }
} finally {
  rmSync(root, { recursive: true, force: true });
}
process.stdout.write(`Zed release package verified: ${version}\n`);
