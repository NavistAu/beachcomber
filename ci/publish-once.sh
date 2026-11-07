#!/bin/sh
# publish-once.sh URL CMD...: run a publish command. If it fails, pass only when
# URL (the registry's page for this exact version) shows it is already published,
# so a rerun is idempotent but a rejected upload fails the job.
url=$1
shift
"$@" && exit 0
code=$(curl -sL -o /dev/null -w '%{http_code}' -A beachcomber-release "$url")
if [ "$code" = 200 ]; then
  echo "Already published: $url"
  exit 0
fi
echo "Publish failed, and $url answers $code" >&2
exit 1
