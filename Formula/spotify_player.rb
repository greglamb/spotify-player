# Homebrew formula for this fork, installing the release binaries built by
# .github/workflows/cd.yml. `version` and the checksums are rewritten on every
# tagged release by scripts/update-formula.py; everything else is hand-edited.
#
# The Linux binaries are built on GitHub's Ubuntu runners and link the system
# ALSA, D-Bus and OpenSSL libraries rather than Homebrew's.
class SpotifyPlayer < Formula
  desc "Terminal Spotify player (greglamb fork)"
  homepage "https://github.com/greglamb/spotify-player"
  version "0.25.1-gh7"
  license "MIT"

  on_macos do
    # the fork publishes Apple silicon binaries only
    depends_on arch: :arm64

    url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-apple-darwin.tar.gz"
    sha256 "d33f036b75cbbb4f5318d3786e786c3bff66c3605198bc675deceadd17ae19d7"
  end

  on_linux do
    on_arm do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c142388bd7c9b544b0730499521f148387f079bd25dfb04b59ff3a7b1deb4b12"
    end

    on_intel do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9925f5e59d5a8d8917bf98586d2b92e733183f22b2ca1f9641b76ea1c3731f71"
    end
  end

  def install
    bin.install "spotify_player"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/spotify_player --version")
  end
end
