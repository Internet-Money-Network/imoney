"""Assembles the public website: the landing page, the explorer and the wallet, plus a small
function that passes /api requests to a public node.

    python scripts/build-site.py --apps-from v0.1.4
    cd dist-site && npx wrangler pages deploy public --project-name internetmoneynetwork

The explorer, wallet and pool pages must match what the public nodes run: a wallet built for a
newer transaction format signs payments those nodes refuse. `--apps-from <git tag>` takes those
pages from the release the nodes run, while the landing page always comes from the working
tree. Without it, everything comes from the working tree.

The explorer and wallet are the same files a node serves; on the website they talk to the
node behind /api instead of the visitor's own.
"""
import argparse
import os
import shutil
import subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "dist-site")
PUBLIC = os.path.join(OUT, "public")

# The node the website's explorer and wallet read from. Its API port is open to Cloudflare only.
UPSTREAM = "http://seed1.internetmoneynetwork.org:18556"

# The pool, whose status page appears on the website under /pool
POOL_UPSTREAM = "http://pool.internetmoneynetwork.org:18558"

POOL_FUNCTION = """// Passes the pool page's data requests (/pool/api/...) to the pool's own status server.
const UPSTREAM = '%s';

export async function onRequest({ request }) {
  const url = new URL(request.url);
  if (request.method !== 'GET' || !url.pathname.startsWith('/pool/api/')) {
    return new Response('Not found', { status: 404 });
  }
  try {
    return await fetch(UPSTREAM + url.pathname.slice('/pool'.length), { headers: { Accept: 'application/json' } });
  } catch (error) {
    return new Response(JSON.stringify({ error: 'The pool is not reachable right now' }), {
      status: 502,
      headers: { 'Content-Type': 'application/json' },
    });
  }
}
""" % POOL_UPSTREAM

FUNCTION = """// Passes the website's /api requests to a public Internet Money node, so the explorer and
// wallet pages work over HTTPS from the same origin. Read-only calls and signed payments
// only: adding blocks is refused here.
const UPSTREAM = '%s';

export async function onRequest({ request }) {
  const url = new URL(request.url);
  if (!url.pathname.startsWith('/api/v1/')) {
    return new Response('Not found', { status: 404 });
  }
  if (url.pathname.startsWith('/api/v1/mining/submit')) {
    return new Response('Mine through your own node or a pool', { status: 403 });
  }
  try {
    return await fetch(new Request(UPSTREAM + url.pathname + url.search, request));
  } catch (error) {
    return new Response(JSON.stringify({ error: 'The public node is not reachable right now' }), {
      status: 502,
      headers: { 'Content-Type': 'application/json' },
    });
  }
}
""" % UPSTREAM


def copy(source, target, ref=None):
    """Copies a file from the working tree, or from git revision `ref` when given."""
    destination = os.path.join(PUBLIC, target)
    os.makedirs(os.path.dirname(destination), exist_ok=True)
    if ref is None:
        shutil.copyfile(os.path.join(ROOT, source), destination)
    else:
        content = subprocess.run(["git", "show", f"{ref}:{source.replace(os.sep, '/')}"], cwd=ROOT, check=True, capture_output=True).stdout
        with open(destination, "wb") as f:
            f.write(content)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--apps-from", metavar="REF", help="take the explorer, wallet and pool pages from this git tag")
    ref = parser.parse_args().apps_from
    shutil.rmtree(OUT, ignore_errors=True)
    for name in ("index.html", "features.html", "whitepaper.html", "internet-money-whitepaper.pdf", "icon.svg", "banner.png"):
        copy(os.path.join("apps", "imoney-website", name), name)
    copy(os.path.join("apps", "imoney-explorer", "index.html"), os.path.join("explorer", "index.html"), ref)
    copy(os.path.join("apps", "imoney-wallet", "index.html"), os.path.join("wallet", "index.html"), ref)
    for name in ("imoney_wasm.js", "imoney_wasm_bg.wasm"):
        copy(os.path.join("apps", "imoney-wallet", "pkg", name), os.path.join("wallet", "pkg", name), ref)
    copy(os.path.join("packages", "imoney-sdk", "dist", "imoney.js"), os.path.join("wallet", "sdk.js"), ref)
    copy(os.path.join("apps", "imoney-pool", "index.html"), os.path.join("pool", "index.html"), ref)

    functions = os.path.join(OUT, "functions", "api")
    os.makedirs(functions, exist_ok=True)
    with open(os.path.join(functions, "[[path]].js"), "w", encoding="utf-8", newline="\n") as f:
        f.write(FUNCTION)
    pool_functions = os.path.join(OUT, "functions", "pool", "api")
    os.makedirs(pool_functions, exist_ok=True)
    with open(os.path.join(pool_functions, "[[path]].js"), "w", encoding="utf-8", newline="\n") as f:
        f.write(POOL_FUNCTION)
    print("built", OUT)


if __name__ == "__main__":
    main()
