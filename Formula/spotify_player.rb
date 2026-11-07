# Homebrew formula for this fork, installing the release binaries built by
# .github/workflows/cd.yml. `version` and the checksums are rewritten on every
# tagged release by scripts/update-formula.py; everything else is hand-edited.
#
# The Linux binaries are built on GitHub's Ubuntu runners and link the system
# ALSA, D-Bus and OpenSSL libraries rather than Homebrew's.
class SpotifyPlayer < Formula
  desc "Terminal Spotify player (greglamb fork)"
  homepage "https://github.com/greglamb/spotify-player"
  version "0.25.1-gh14"
  license "MIT"

  on_macos do
    # the fork publishes Apple silicon binaries only
    depends_on arch: :arm64

    url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-apple-darwin.tar.gz"
    sha256 "975f2ae98054134865d8cf9b65b97781aeee2edd9c053993829bd99b1a75be98"
  end

  on_linux do
    on_arm do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "74c4985217048f4ef2f81401e2d92ba001558ea48915041022bda8d2d94281b6"
    end

    on_intel do
      url "https://github.com/greglamb/spotify-player/releases/download/v#{version}/spotify_player-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a99a5b95753feacdb154a5b383719237e96befe5c798873711ccc04dc6914899"
    end
  end

  def install
    bin.install "spotify_player"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/spotify_player --version")
  end
end
