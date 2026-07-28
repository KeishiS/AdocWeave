import { build } from "esbuild";

await build({
  bundle: true,
  entryPoints: ["src/extension.ts"],
  external: ["vscode"],
  format: "cjs",
  logLevel: "info",
  outfile: "dist/extension.cjs",
  platform: "node",
  sourcemap: false,
  target: "node20",
});
