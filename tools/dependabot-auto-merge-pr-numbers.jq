def valid_pull_request:
  type == "object"
  and (.number | type == "number" and . > 0 and floor == .)
  and (.user.login | type == "string")
  and (.base.ref | type == "string")
  and (.base.repo.full_name | type == "string")
  and (.head.ref | type == "string")
  and (.head.repo.full_name | type == "string");

if length > 0 and all(.[]; type == "array")
then add
else error("Pull Request response must contain one or more arrays")
end
| if all(.[]; valid_pull_request)
  then .
  else error("Pull Request response contains an invalid entry")
  end
| map(select(
    .user.login == "dependabot[bot]"
    and .base.ref == "main"
    and .base.repo.full_name == env.GITHUB_REPOSITORY
    and .head.repo.full_name == env.GITHUB_REPOSITORY
    and (.head.ref | startswith("dependabot/"))
  ))
| map(.number)
