#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
  echo "usage: dependabot-alert-inventory.sh OWNER/REPOSITORY" >&2
  exit 2
fi

gh api --paginate \
  "repos/$1/dependabot/alerts?state=open&per_page=100" \
  | jq -es '
      if length > 0 and all(.[]; type == "array")
      then add | length
      else error("Dependabot alert response must contain one or more arrays")
      end
    '
