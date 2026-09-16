# Homebrew formula for Theta.
#
# This file is kept in the main repo as the source of truth, but Homebrew can
# only install it from a *tap* — a separate repository named `homebrew-tap`.
# Copy this file to `Formula/theta.rb` in that repository to publish it. See
# README.md in this directory for the one-time setup.
#
# `sha256` values must match the release artifacts. The release workflow prints
# them (`SHA256SUMS`) — see packaging/homebrew/update-formula.sh to fill them in.
class Theta < Formula
  desc "Multi-session tiled terminal workspace for coding agents"
  homepage "https://github.com/Bhanu4417/theta"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Bhanu4417/theta/releases/download/v#{version}/theta-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    end
    on_intel do
      url "https://github.com/Bhanu4417/theta/releases/download/v#{version}/theta-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Bhanu4417/theta/releases/download/v#{version}/theta-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_LINUX_SHA256"
    end
    on_intel do
      url "https://github.com/Bhanu4417/theta/releases/download/v#{version}/theta-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_X86_64_LINUX_SHA256"
    end
  end

  def install
    bin.install "theta"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/theta --version")
  end
end
