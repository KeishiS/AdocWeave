import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

const tsc = fileURLToPath(new URL("../node_modules/typescript/bin/tsc", import.meta.url));
execFileSync(process.execPath, [tsc, "-p", "tsconfig.test.json"], { stdio: "inherit" });
mkdirSync("dist-test/resources", { recursive: true });
copyFileSync("resources/platforms.json", "dist-test/resources/platforms.json");
mkdirSync("dist-test/syntaxes", { recursive: true });
copyFileSync("syntaxes/asciidoc.tmLanguage.json", "dist-test/syntaxes/asciidoc.tmLanguage.json");
mkdirSync("dist-test/test/fixtures", { recursive: true });
copyFileSync("test/fixtures/grammar-scopes.json", "dist-test/test/fixtures/grammar-scopes.json");
