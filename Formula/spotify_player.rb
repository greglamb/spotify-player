# Homebrew formula for this fork, installing the release binaries built by
# .github/workflows/cd.yml. `version` and the checksums are rewritten on every
# tagged release by scripts/update-formula.py; everything else is hand-edited.
#
# The Linux binaries are built on GitHub's Ubuntu runners and link the system
# ALSA, D-Bus and OpenSSL libraries rather than Homebrew's.
class SpotifyPlayer < Formula
  desc "Terminal Spotify player (greglamb fork)"
  homepage "https://github.com/greglamb/spotify-player"
  version "0.25.1-gh6"
  license "MIT"

  on_macos do
    # the fork publishes Apple silicon binaries only
    depends_on arch: :arm64

    url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-apple-darwin.tar.gz"
    sha256 "9b8d7394c4561e1d46fdf5463f6dea01312f39d080db90f8fa2aa070a54650a8"
  end

  on_linux do
    on_arm do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "00c4b44d469022ad6081b595cc0a77ee49dcf512b9e8c40e26147b905c75c91e"
    end

    on_intel do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ecaa15fe50e0e2a1267859d0f5de719af3a84cb5c7bca23cdb064a8cdee791f1"
    end
  end

  def install
    bin.install "spotify_player"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/spotify_player --version")
  end
end
