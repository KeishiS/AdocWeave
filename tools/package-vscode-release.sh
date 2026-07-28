#!/usr/bin/env bash
set -euo pipefail

npm ci --ignore-scripts --prefix editors/vscode
npm run check --prefix editors/vscode
npm test --prefix editors/vscode
npm run package --prefix editors/vscode
npm run test:vsix --prefix editors/vscode
