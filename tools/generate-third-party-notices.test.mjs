import assert from "node:assert/strict";
import test from "node:test";

import { renderThirdPartyNotices, thirdPartyPackages } from "./generate-third-party-notices.mjs";

const workspace = { id: "adocweave 0.6.3 (path+file:///workspace)", name: "adocweave", version: "0.6.3" };
const packageOf = (name, version, license) => ({ id: `${name} ${version} (registry+https://example.invalid)`, name, version, license });

test("notice rendering groups root dependencies and leaves shared Zed dependencies out of the extension section", () => {
  const root = {
    workspace_members: [workspace.id],
    packages: [workspace, packageOf("alpha", "1.0.0", "MIT"), packageOf("beta", "2.0.0", "Apache-2.0")],
  };
  const zed = {
    workspace_members: [workspace.id],
    packages: [workspace, packageOf("alpha", "1.0.0", "MIT"), packageOf("gamma", "3.0.0", "MIT")],
  };

  const rendered = renderThirdPartyNotices(root, zed);
  assert.match(rendered, /\|Apache-2\.0\n\|beta 2\.0\.0/);
  assert.match(rendered, /\|MIT\n\|alpha 1\.0\.0/);
  assert.match(rendered, /== Zed開発拡張archiveの追加依存[\s\S]*\|MIT\n\|gamma 3\.0\.0/);
  assert.doesNotMatch(rendered, /== Zed開発拡張archiveの追加依存[\s\S]*alpha 1\.0\.0/);
});

test("notice rendering rejects dependencies without SPDX license metadata", () => {
  const metadata = { workspace_members: [workspace.id], packages: [workspace, packageOf("missing", "1.0.0", null)] };
  assert.throws(() => thirdPartyPackages(metadata), /missing 1\.0\.0 has no license metadata/);
});
