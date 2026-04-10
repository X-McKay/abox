# Bakudo Split Status

The `harness/` project has been split into an independent git repository at:

- `/workspace/bakudo`

Initialization details:

- Repository initialized with branch `main`
- Initial commit: `e4025fd`

## Push from an `abox` branch to external repo

Use the helper script from the `abox` repo root:

```bash
harness/scripts/push-harness-to-repo.sh https://github.com/X-McKay/bakudo.git work main
```

This will:

1. Create a temporary subtree-split branch from `harness/` on `work`
2. Push that split branch to `main` in `https://github.com/X-McKay/bakudo.git`
3. Clean up the temporary local split branch

## Manual equivalent command

```bash
git subtree split --prefix=harness work -b bakudo-sync
git push https://github.com/X-McKay/bakudo.git bakudo-sync:main
git branch -D bakudo-sync
```
