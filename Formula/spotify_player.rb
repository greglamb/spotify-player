# Homebrew formula for this fork, installing the release binaries built by
# .github/workflows/cd.yml. `version` and the checksums are rewritten on every
# tagged release by scripts/update-formula.py; everything else is hand-edited.
#
# The Linux binaries are built on GitHub's Ubuntu runners and link the system
# ALSA, D-Bus and OpenSSL libraries rather than Homebrew's.
class SpotifyPlayer < Formula
  desc "Terminal Spotify player (greglamb fork)"
  homepage "https://github.com/greglamb/spotify-player"
  version "0.25.1-gh3"
  license "MIT"

  on_macos do
    # the fork publishes Apple silicon binaries only
    depends_on arch: :arm64

    url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-apple-darwin.tar.gz"
    sha256 "255e24624d41cf2908d3a5c8c16d522e95621a57431ffd211c3c585ad8fe7aec"
  end

  on_linux do
    on_arm do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "142b98d94c98a557754c2628951812a2ee822094705ddecd8f80efe418a6a59e"
    end

    on_intel do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b30c211ca85e78e77cf5dd21b4f1c269ca872b60ca6b103250027894442d5d5d"
    end
  end

  def install
    bin.install "spotify_player"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/spotify_player --version")
  end
end
