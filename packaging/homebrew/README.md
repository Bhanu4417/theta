# Homebrew

`brew install` reads formulas from a **tap**: a repository whose name starts
with `homebrew-`. Theta's formula lives here so it is versioned with the code,
but it only becomes installable once a tap repository publishes it.

## One-time setup

1. Create a public repository named **`homebrew-tap`** under the same account
   (`Bhanu4417/homebrew-tap`).
2. In that repository, add `Formula/theta.rb` containing the formula from this
   directory, with the `sha256` values filled in.
3. Users can then run:

   ```sh
   brew tap Bhanu4417/tap
   brew install theta
   ```

## Filling in the checksums

After a release is published, its artifacts and hashes are on the release page
and in the `SHA256SUMS` asset:

```sh
curl -fsSL https://github.com/Bhanu4417/theta/releases/download/v0.1.0/SHA256SUMS
```

`update-formula.sh` in this directory does it for you — it takes a version and
rewrites the `version` and all four `sha256` lines:

```sh
./update-formula.sh 0.1.0        # reads the published SHA256SUMS
```

Run it, then copy the result into `Formula/theta.rb` in the tap repository.

## Automating it

Once the tap exists, add a step to the release workflow that runs
`update-formula.sh` and pushes the result to the tap with a token that has
write access to it. That keeps the formula current on every tag with no manual
step.
