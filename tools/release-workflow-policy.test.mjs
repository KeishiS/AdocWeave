import assert from "node:assert/strict";
import test from "node:test";

import {
  loadWorkflowPolicyInputs,
  validatePinnedActions,
  validateReleaseWorkflowPolicy,
} from "./release-workflow-policy.mjs";

test("repository release workflows satisfy the least-privilege policy", () => {
  validateReleaseWorkflowPolicy(loadWorkflowPolicyInputs());
});

test("every external action requires a full commit SHA", () => {
  assert.throws(
    () => validatePinnedActions({
      "unsafe.yml": "jobs:\n  unsafe:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v7\n",
    }),
    /not pinned/,
  );
});

test("build and publish workflows cannot receive repository secrets", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, release: `${inputs.release}\nsecrets: inherit\n` }),
    /must not receive repository secrets/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, publish: `${inputs.publish}\nsecrets: inherit\n` }),
    /scoped GitHub token/,
  );
});

test("publisher cannot omit its protected environment or cleanup", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      publish: inputs.publish.replace("environment: github-release", "environment: unprotected"),
    }),
    /protected github-release environment/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      publish: inputs.publish.replace("if: failure()", "if: success()"),
    }),
    /clean up its draft/,
  );
});

test("tag runs cannot be cancelled with superseded source runs", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "${{ !startsWith(github.ref, 'refs/tags/') }}",
        "${{ true }}",
      ),
    }),
    /without cancelling tags/,
  );
});

test("only stable tags trigger publication and tag-only steps stay structurally isolated", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "- 'v[0-9]+.[0-9]+.[0-9]+'",
        "- 'v*'",
      ),
    }),
    /only for stable semantic version tags/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "      - id: candidate\n        if: startsWith(github.ref, 'refs/tags/')",
        "      - id: candidate\n        if: always()",
      ),
    }),
    /candidate lookup must be structurally limited to tags/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "      - name: Publication tag verification against the current main commit\n        if: startsWith(github.ref, 'refs/tags/')",
        "      - name: Publication tag verification against the current main commit\n        if: always()",
      ),
    }),
    /tag verification must be structurally limited to tags/,
  );
});

test("fast planner cannot wait for Nix or omit main change detection", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "      - id: changes",
        "      - uses: DeterminateSystems/determinate-nix-action@d96678350ffd6a456235832eb11e1c491589b7bb\n      - id: changes",
      ),
    }),
    /must not wait for Nix/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        'git diff --name-only "$BEFORE_SHA" "$GITHUB_SHA"',
        ": # omitted main diff",
      ),
    }),
    /main planning/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        'git show-ref --verify --quiet "refs/tags/v$version"',
        "false # omitted release intent",
      ),
    }),
    /release intent/,
  );
});

test("candidate jobs cannot broaden the explicit artifact plan", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "if: needs.changes.outputs.global_required == 'true'",
        "if: github.event_name == 'push'",
      ),
    }),
    /explicit candidate change plan/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "if: needs.changes.outputs.release_main == 'true'",
        "if: github.ref == 'refs/heads/main'",
      ),
    }),
    /release-intent main/,
  );
});

test("release workflow cannot cache executable build tools", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "      - name: Windows target archive builds",
        "      - uses: actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830\n" +
        "        with:\n" +
        "          path: target/cargo-dist-bin\n" +
        "          key: executable\n" +
        "      - name: Windows target archive builds",
      ),
    }),
    /must not cache executable build tools/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "./tools/install-pinned-cargo-dist.ps1",
        "cargo install cargo-dist",
      ),
    }),
    /reviewed bootstrap|registry build/,
  );
});

test("Windows cargo-dist bootstrap pins the complete reviewed asset identity", () => {
  const inputs = loadWorkflowPolicyInputs();
  for (const mutation of [
    { url: "https://github.com/axodotdev/cargo-dist/releases/latest/download/cargo-dist-x86_64-pc-windows-msvc.zip" },
    { sha256: "0".repeat(64) },
    { asset: "cargo-dist-installer.ps1" },
    { archiveEntries: [...inputs.windowsDistBootstrap.archiveEntries, "unexpected.exe"] },
    { executable: "cargo-dist.exe" },
  ]) {
    assert.throws(
      () => validateReleaseWorkflowPolicy({
        ...inputs,
        windowsDistBootstrap: { ...inputs.windowsDistBootstrap, ...mutation },
      }),
      /exactly pin the reviewed release asset/,
    );
  }
});

test("Windows cargo-dist bootstrap cannot omit bounded download and extraction", () => {
  const inputs = loadWorkflowPolicyInputs();
  for (const argument of ["-DownloadTimeoutSeconds 60", "-ExtractionTimeoutSeconds 30"]) {
    assert.throws(
      () => validateReleaseWorkflowPolicy({
        ...inputs,
        release: inputs.release.replace(argument, ""),
      }),
      /must have a timeout/,
    );
  }
});

test("Windows cargo-dist bootstrap rejects unsafe extraction mutations", () => {
  const inputs = loadWorkflowPolicyInputs();
  for (const [original, replacement] of [
    ['$actualHash -cne $config.sha256', "$false"],
    [
      "Compare-Object -CaseSensitive $actualEntries $expectedEntries",
      "Compare-Object -CaseSensitive $actualEntries $actualEntries",
    ],
    ["GetFileName($entryName) -cne $entryName", "$false"],
    ["$archive.GetEntry($config.executable)", "$archive.Entries[0]"],
    [
      "Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force",
      "Write-Output $temporaryDirectory",
    ],
  ]) {
    assert.throws(
      () => validateReleaseWorkflowPolicy({
        ...inputs,
        windowsDistInstaller: inputs.windowsDistInstaller.replace(original, replacement),
      }),
      /bootstrap must/,
    );
  }
});

test("Windows cargo-dist bootstrap verifies before opening and extracting", () => {
  const inputs = loadWorkflowPolicyInputs();
  const installer = inputs.windowsDistInstaller
    .replace(
      "  $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)",
      "  # archive opening moved",
    )
    .replace(
      '  if ($actualHash -cne $config.sha256) {',
      "  $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)\n" +
      '  if ($actualHash -cne $config.sha256) {',
    );
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, windowsDistInstaller: installer }),
    /verify hash and entries before extraction/,
  );
});

test("quality required check aggregates every canonical local unit", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      contract: inputs.contract.replace(
        "needs: [source-fast, rust, adapters, dependencies, fuzz, nix-package]",
        "needs: [source-fast, rust, adapters, fuzz, nix-package]",
      ),
    }),
    /aggregate every local gate unit/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      contract: inputs.contract.replace(
        "nix develop .#ci -c cargo make quality-fast",
        "true # quality-fast",
      ),
    }),
    /source-fast must use its canonical local task/,
  );
});

test("Makefile canonical gate graph is parsed and mutation-resistant", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      makefile: inputs.makefile.replace(
        'dependencies = ["quality-fast", "quality-rust", "quality-adapters"]',
        'dependencies = ["quality-fast", "quality-rust"]',
      ),
    }),
    /quality dependencies must exactly match/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      makefile: inputs.makefile.replace(
        '  "dependency-governance",\n',
        "",
      ),
    }),
    /verify dependencies must exactly match/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      makefile: inputs.makefile.replace(
        '[tasks.ci]\ndescription = "Alias for the canonical local pull request gate"\nalias = "verify"',
        '[tasks.ci]\ndescription = "Alias for the canonical local pull request gate"\nalias = "quality"',
      ),
    }),
    /ci must alias verify/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      makefile: inputs.makefile.replace(
        '  "test-cross-runtime",\n]',
        '  "test-cross-runtime",\n  "check",\n]',
      ),
    }),
    /quality-adapters dependencies must exactly match/,
  );
});

test("tag publication must reuse and verify the selected main candidate", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(".[].workflow_runs[]", ".[][]"),
    }),
    /traverse response pages/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "run-id: ${{ needs.release-plan.outputs.candidate_run_id }}",
        "run-id: ${{ github.run_id }}",
      ),
    }),
    /selected main candidate/,
  );
});

test("network installer cannot replace the locked cargo-dist closure", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "nix develop .#ci -c tools/run-pinned-dist.sh build",
        "curl https://example.invalid/installer.sh | sh",
      ),
    }),
    /locked build closure|bypass/,
  );
});

test("native smoke cannot repeat source adapter tests", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      smoke: inputs.smoke.replace(
        "      - name: Extracted release binary smoke tests",
        "      - run: npm test --prefix editors/vscode\n      - name: Extracted release binary smoke tests",
      ),
    }),
    /must not repeat source adapter tests/,
  );
});

test("private draft publication must use the returned upload URL and avoid tag-only APIs", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      publish: inputs.publish.replace(
        '"$upload_url?name=$name"',
        '"https://uploads.github.invalid/fixed?name=$name"',
      ),
    }),
    /returned draft URL/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      publish: inputs.publish.replace(
        "          tag=\"$(jq -r '.announcement_tag' <<<\"$PLAN\")\"",
        "          tag=\"$(jq -r '.announcement_tag' <<<\"$PLAN\")\"\n" +
        "          gh api \"repos/$GITHUB_REPOSITORY/releases/tags/$tag\"",
      ),
    }),
    /tag-only release API/,
  );
});
