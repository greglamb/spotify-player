# Homebrew formula for this fork, installing the release binaries built by
# .github/workflows/cd.yml. `version` and the checksums are rewritten on every
# tagged release by scripts/update-formula.py; everything else is hand-edited.
#
# The Linux binaries are built on GitHub's Ubuntu runners and link the system
# ALSA, D-Bus and OpenSSL libraries rather than Homebrew's.
class SpotifyPlayer < Formula
  desc "Terminal Spotify player (greglamb fork)"
  homepage "https://github.com/greglamb/spotify-player"
  version "0.25.1-gh2"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-apple-darwin.tar.gz"
      sha256 "b21ebf1d91802d0bf5065ba4ac3d614d96e3777462ce952b77cfc9acc5762efd"
    end

    on_intel do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-x86_64-apple-darwin.tar.gz"
      sha256 "628317d9554497a728fc470040b6c075f2bd81c0de9f316f4f32ff6df2634ee8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3e2a79189517ba5e4588c050d62549d8575015779f2f207309d8781e24de7fef"
    end

    on_intel do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5c7e83118d04deade16aa1103d393411800d61d28485c54270c6f0a8f2302ec6"
    end
  end

  def install
    bin.install "spotify_player"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/spotify_player --version")
  end
end
