#!/usr/bin/env bash
# Deploy Screeny Studio to a Docker host over SSH.
#
#   tools/deploy.sh --dry-run        # print the whole plan, run nothing
#   tools/deploy.sh                  # pull on the host, build, up -d
#   tools/deploy.sh --status         # is it up? is it healthy?
#   tools/deploy.sh --logs           # what has it been saying?
#   tools/deploy.sh --down           # stop and remove the stack
#
# What it does, and nothing else: read-only `git` locally, `ssh` to the deploy
# host, and `docker compose` inside one checkout directory on that host with an
# explicit project name (`screeny`) and an explicit `-f` file. The project name
# and `-f` file are what keep this stack out of the way of anything else
# running on the host: they never touch another container, never change Docker
# daemon settings, never prune, never open a firewall port.
#
# **This script never pushes anything, anywhere.** Pushing is a deliberate,
# separate step somebody does by hand. What the script does instead is refuse to
# run when the branch it would deploy is not on the remote yet, and say which
# push to do - because deploying an old commit silently is a trap.
#
# Two routes to get the code onto the host:
#
#   github (default)  the host clones/pulls the git remote `origin` itself.
#   deploy             a bare repo on the host, reached as the git remote
#                     `deploy`. Nothing goes near GitHub (or wherever `origin`
#                     points). Kept for a host with no access to `origin`, and
#                     for a repo that is not meant to leave this machine yet.
#
# See `docs/design/deployment.md` for the runbook and for what to check on the
# host afterwards.

set -euo pipefail

# ------------------------------------------------------------------ defaults

HOST=${SCREENY_DEPLOY_HOST:-}
ROUTE=github
GITHUB_URL=${SCREENY_GITHUB_URL:-$(git remote get-url origin 2>/dev/null || true)}
REMOTE_NAME=origin
# Where the checkout and the bare repo live on the deploy host. `$HOME` is the
# *remote* shell's, and must reach it unexpanded, so the single quotes are the
# point and SC2016 is exactly backwards here.
# shellcheck disable=SC2016
DIR=${SCREENY_DEPLOY_DIR:-'$HOME/src/screeny'}
# shellcheck disable=SC2016
BARE='$HOME/srv/screeny.git'
PROJECT=screeny
COMPOSE_FILE=docker-compose.yml
PORT=${SCREENY_PORT:-8787}
BRANCH=main
# The host's `render` group, which owns /dev/dri/renderD128. There is no
# universal default; `stat -c %g /dev/dri/renderD128` on the host says what it
# is there.
RENDER_GID=${SCREENY_RENDER_GID:-993}
# The host's own timezone may not be the timezone the clock patches should
# show, so the container is told explicitly.
TZ_NAME=${SCREENY_TZ:-America/Los_Angeles}

DRY_RUN=0
DO_BUILD=1
ACTION=deploy
DOWN_VOLUMES=0

usage() {
  cat <<'EOF'
tools/deploy.sh - deploy Screeny Studio to a Docker host over SSH

  SCREENY_DEPLOY_HOST must be set (or pass --host): the ssh target. There is
  no default - deploying to the wrong box by accident is worse than refusing.

  --route github|deploy     where the host gets the code from.
                            github (default): the host pulls from the local
                              `origin` remote's URL ($SCREENY_GITHUB_URL
                              overrides it)
                            deploy: a bare repo on the host, pushed to as
                              the git remote `deploy`. No GitHub involved.
  --host HOST               ssh target (required: $SCREENY_DEPLOY_HOST, or this flag)
  --dir PATH                checkout on the host (default: $HOME/src/screeny,
                            or $SCREENY_DEPLOY_DIR)
  --bare PATH               bare repo on the host, deploy route
                            (default: $HOME/srv/screeny.git)
  --port N                  web port on the host (default: 8787, or $SCREENY_PORT)
  --render-gid N            the host's `render` group, which owns /dev/dri
                            (default: 993, or $SCREENY_RENDER_GID; find it with
                            `stat -c %g /dev/dri/renderD128`)
  --tz ZONE                 timezone for the container (default:
                            America/Los_Angeles, or $SCREENY_TZ)
  --branch NAME             branch to deploy (default: main)
  --no-build                skip `docker compose build`; just `up -d`
  --dry-run                 print every command, local and remote, and run none
  --status                  print `docker compose ps` and /healthz, change nothing
  --logs                    print the last 200 lines of the studio's log
  --down                    stop and remove the stack (the state volume survives)
  --down --volumes          ... and remove the state volume too
  -h, --help                this

This script never pushes. If the branch is not on the remote yet it stops and
tells you which push to run.

Exit codes: 0 fine, 1 usage or refusal, 2 something on the host failed.
EOF
}

# ------------------------------------------------------------------ plumbing

say()  { printf '%s\n' "$*"; }
warn() { printf 'deploy: %s\n' "$*" >&2; }
step() { printf '\n== %s\n' "$*"; }

# A refusal is not a crash: say what is wrong, then what to run about it.
refuse() {
  printf '\ndeploy: refusing.\n\n  %s\n\n' "$1" >&2
  shift
  if [ "$#" -gt 0 ]; then
    printf 'What to do:\n\n' >&2
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
    printf '\n' >&2
  fi
  exit 1
}

die() { printf '\ndeploy: %s\n\n' "$1" >&2; exit 2; }

# Every command this script runs is printed first, local or remote, dry run or
# not, so that what happened on the host is in the scrollback afterwards.
run_local() {
  printf '  + %s\n' "$*"
  if [ "$DRY_RUN" -eq 0 ]; then "$@"; fi
}

# The remote side takes one shell string on purpose: these are pipelines and
# `cd`s, and pretending otherwise would be a lie about what runs. Nothing here
# ever ssh's when DRY_RUN is set - the print is the whole effect.
run_remote() {
  printf '  + ssh %s -- %s\n' "$HOST" "$1"
  if [ "$DRY_RUN" -eq 0 ]; then
    ssh -o BatchMode=yes "$HOST" "$1"
  fi
}

# `docker compose` on the host: always the project name, always the file, and
# always the same environment. The environment has to be on the `docker compose`
# command itself and not in front of the `cd`: `cd` is a regular builtin, so a
# prefix assignment there would not survive to the next command in the list.
# Every subcommand gets it, because `ps` and `down` have to compute the same
# config that `up` did.
compose() {
  printf 'cd %s && SCREENY_PORT=%s SCREENY_RENDER_GID=%s TZ=%s docker compose -p %s -f %s %s' \
    "$DIR" "$PORT" "$RENDER_GID" "$TZ_NAME" "$PROJECT" "$COMPOSE_FILE" "$*"
}

# --------------------------------------------------------------- arguments

while [ "$#" -gt 0 ]; do
  case $1 in
    --route)    ROUTE=${2:?--route needs github or deploy}; shift 2 ;;
    --host)     HOST=${2:?--host needs a name}; shift 2 ;;
    --dir)      DIR=${2:?--dir needs a path}; shift 2 ;;
    --bare)     BARE=${2:?--bare needs a path}; shift 2 ;;
    --port)     PORT=${2:?--port needs a number}; shift 2 ;;
    --render-gid) RENDER_GID=${2:?--render-gid needs a number}; shift 2 ;;
    --tz)       TZ_NAME=${2:?--tz needs a zone name}; shift 2 ;;
    --branch)   BRANCH=${2:?--branch needs a name}; shift 2 ;;
    --no-build) DO_BUILD=0; shift ;;
    --dry-run)  DRY_RUN=1; shift ;;
    --status)   ACTION=status; shift ;;
    --logs)     ACTION=logs; shift ;;
    --down)     ACTION=down; shift ;;
    --volumes)  DOWN_VOLUMES=1; shift ;;
    -h|--help)  usage; exit 0 ;;
    *) warn "unknown argument $1"; usage >&2; exit 1 ;;
  esac
done

if [ -z "$HOST" ]; then
  usage >&2
  warn "SCREENY_DEPLOY_HOST is not set, and no --host was given. Nothing was run."
  exit 1
fi

case $ROUTE in
  github)  REMOTE_NAME=origin ;;
  deploy)  REMOTE_NAME=deploy ;;
  *) refuse "--route $ROUTE is not a route." \
       "--route github   (the host pulls from origin; the default)" \
       "--route deploy   (a bare repo on $HOST; no GitHub)" ;;
esac

if [ "$DRY_RUN" -eq 1 ]; then
  say "DRY RUN - every command below is printed and none of them is run."
fi

say ""
say "host        $HOST"
say "route       $ROUTE   (git remote '$REMOTE_NAME')"
say "branch      $BRANCH"
say "checkout    $DIR   (on $HOST)"
if [ "$ROUTE" = deploy ]; then
  say "bare repo   $BARE   (on $HOST)"
else
  say "source      $GITHUB_URL"
fi
say "project     $PROJECT        compose file  $COMPOSE_FILE"
say "render gid  $RENDER_GID        TZ  $TZ_NAME"
say "web         http://$HOST:$PORT/"

# ------------------------------------------------------------- side actions

if [ "$ACTION" = status ]; then
  step "the stack"
  run_remote "$(compose ps)"
  step "health"
  run_remote "curl -fsS http://127.0.0.1:$PORT/healthz || echo 'NOT ANSWERING on 127.0.0.1:$PORT'"
  exit 0
fi

if [ "$ACTION" = logs ]; then
  step "the studio's log, last 200 lines"
  run_remote "$(compose "logs --tail 200 studio")"
  exit 0
fi

if [ "$ACTION" = down ]; then
  step "stopping the stack"
  if [ "$DOWN_VOLUMES" -eq 1 ]; then
    say "  --volumes given: the state volume goes too, so what was playing is forgotten."
    run_remote "$(compose "down --volumes")"
  else
    run_remote "$(compose down)"
  fi
  say ""
  say "The image and (unless --volumes) the state volume are still on $HOST:"
  say "  ssh $HOST -- docker image rm screeny-studio:local"
  say "  ssh $HOST -- docker volume rm ${PROJECT}_state"
  exit 0
fi

# -------------------------------------------------------- local preflight

step "local preflight"

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || refuse \
  "not inside a git repository." "Run this from the screeny checkout."
say "  repo        $REPO_ROOT"

if [ -n "$(git status --porcelain)" ]; then
  warn "the working tree is dirty; only committed, pushed work can be deployed."
fi

git rev-parse --verify --quiet "$BRANCH" >/dev/null || refuse \
  "there is no local branch '$BRANCH'." "git branch -a"

LOCAL_SHA=$(git rev-parse "$BRANCH")
say "  $BRANCH        $LOCAL_SHA"

if [ "$ROUTE" = deploy ]; then
  # The remote must exist and must point at this host, not somewhere else.
  # Adding a remote is a local, reversible thing; repointing an existing one
  # is not, so the script will not do that.
  SSH_URL="$HOST:${BARE#\$HOME/}"
  if git remote get-url "$REMOTE_NAME" >/dev/null 2>&1; then
    HAVE=$(git remote get-url "$REMOTE_NAME")
    say "  remote      $REMOTE_NAME -> $HAVE"
    case $HAVE in
      *github.com*) refuse \
        "the git remote '$REMOTE_NAME' points at GitHub ($HAVE)." \
        "That is not what this route is. Either use the GitHub route:" \
        "  tools/deploy.sh --route github" \
        "or point the remote at the host:" \
        "  git remote set-url $REMOTE_NAME $SSH_URL" ;;
    esac
  else
    say "  remote      $REMOTE_NAME is missing; adding it (local only, nothing is pushed)"
    run_local git remote add "$REMOTE_NAME" "$SSH_URL"
  fi
else
  git remote get-url origin >/dev/null 2>&1 || refuse \
    "there is no 'origin' remote, and --route github deploys what is on origin/$BRANCH." \
    "git remote add origin $GITHUB_URL" \
    "or use --route deploy, which needs no GitHub at all."
  say "  remote      origin -> $(git remote get-url origin)"
fi

# --------------------------------------------------------------- reachable?
#
# Before the ahead/behind check, not after, because on the deploy route the
# refusal below says "git push deploy main" - and that push only works once
# the bare repo on the host exists. Making it exist is the next step.

step "can we reach $HOST?"
run_remote "echo connected as \$(id -un)@\$(hostname) && docker --version && docker compose version --short" \
  || die "cannot ssh to $HOST, or it has no usable docker.
    ssh $HOST -- true                 # is the host there and is your key on it?
    ssh $HOST -- docker ps            # are you in the docker group there?
  --host NAME points this somewhere else."

if [ "$ROUTE" = deploy ]; then
  step "the bare repo on $HOST"
  run_remote "mkdir -p \$(dirname $BARE) && { test -d $BARE || git init --bare -b $BRANCH $BARE; } && echo bare repo: $BARE"
fi

# --------------------------------------------------- is the remote current?

step "does $REMOTE_NAME have $BRANCH?"

if [ "$DRY_RUN" -eq 1 ]; then
  printf '  + git fetch %s %s\n' "$REMOTE_NAME" "$BRANCH"
  say "  (dry run: not fetching, so the ahead/behind check below is skipped.)"
  say "  For real, this step compares $BRANCH with $REMOTE_NAME/$BRANCH and stops"
  say "  here if $BRANCH is ahead, telling you to run: git push $REMOTE_NAME $BRANCH"
else
  git fetch --quiet "$REMOTE_NAME" "$BRANCH" 2>/dev/null || true
  REMOTE_SHA=$(git rev-parse --verify --quiet "refs/remotes/$REMOTE_NAME/$BRANCH" || true)

  if [ -z "$REMOTE_SHA" ]; then
    refuse "$REMOTE_NAME has no '$BRANCH', so there is nothing for $HOST to deploy." \
      "git push $REMOTE_NAME $BRANCH        # then re-run this script" \
      "(this script never pushes for you; that is a decision, not a step)"
  elif [ "$REMOTE_SHA" = "$LOCAL_SHA" ]; then
    say "  $REMOTE_NAME/$BRANCH is exactly $BRANCH. Good."
  else
    AHEAD=$(git rev-list --count "$REMOTE_SHA..$LOCAL_SHA")
    BEHIND=$(git rev-list --count "$LOCAL_SHA..$REMOTE_SHA")
    say "  $REMOTE_NAME/$BRANCH  $REMOTE_SHA"
    say "  local is $AHEAD ahead, $BEHIND behind."
    if [ "$AHEAD" -gt 0 ]; then
      refuse \
        "local $BRANCH is $AHEAD commit(s) ahead of $REMOTE_NAME/$BRANCH, so this would deploy older code." \
        "git push $REMOTE_NAME $BRANCH        # then re-run this script" \
        "(this script never pushes for you; that is a decision, not a step)"
    fi
    if [ "$BEHIND" -gt 0 ]; then
      warn "local $BRANCH is $BEHIND behind $REMOTE_NAME/$BRANCH; the host will get $REMOTE_NAME/$BRANCH, not your tree."
    fi
  fi
fi

# ----------------------------------------------------------------- get code

step "the checkout on $HOST"
if [ "$ROUTE" = deploy ]; then
  run_remote "test -d $DIR/.git || git clone $BARE $DIR"
  run_remote "cd $DIR && git remote set-url origin $BARE && git fetch --prune origin && git checkout -B $BRANCH origin/$BRANCH && git --no-pager log --oneline -1"
else
  run_remote "mkdir -p \$(dirname $DIR) && { test -d $DIR/.git || git clone $GITHUB_URL $DIR; }"
  run_remote "cd $DIR && git fetch --prune origin && git checkout -B $BRANCH origin/$BRANCH && git --no-pager log --oneline -1"
fi

# ------------------------------------------------------------------- deploy

if [ "$DO_BUILD" -eq 1 ]; then
  step "building the image on $HOST (slow the first time: Rust + wgpu + Mesa)"
  run_remote "$(compose build)" \
    || die "the image did not build on $HOST. The error is above; nothing was started,
  and whatever was running before is still running untouched."
fi

step "starting the stack"
run_remote "$(compose 'up -d --remove-orphans')" \
  || die "docker compose up failed on $HOST. The error is above. If it is about
  /dev/dri or a group, check the render gid:
    ssh $HOST -- stat -c '%G %g' /dev/dri/renderD128
    tools/deploy.sh --render-gid <gid>"

step "waiting for it to answer"
run_remote "for i in \$(seq 1 60); do curl -fsS -o /dev/null http://127.0.0.1:$PORT/healthz && { echo \"healthz: ok after \${i}s\"; exit 0; }; sleep 1; done; echo 'healthz: NO ANSWER after 60s'; exit 1" \
  || die "the studio did not answer on $HOST:$PORT within 60s. Look at the log:
    tools/deploy.sh --logs"

step "the stack"
run_remote "$(compose ps)"

cat <<EOF

Deployed.

  http://$HOST:$PORT/     the studio. There is no password - see docs/design/deployment.md.

Three things to check once, on $HOST:

  ssh $HOST -- docker exec screeny-studio screeny discover      # (a) does mDNS work in the container?
  ssh $HOST -- docker exec screeny-studio vulkaninfo --summary  # (b) does it see the GPU?
  ssh $HOST -- docker exec screeny-studio date                  # (c) is the clock local time?

To undo everything this created:

  tools/deploy.sh --down              # stop and remove the container
  tools/deploy.sh --down --volumes    # ... and forget the saved state
EOF
