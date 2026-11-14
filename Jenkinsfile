// Builds the Parla server (Rust binary + Debian package) and publishes the package to the MirraNET APT
// repository (apt.mirranet.de).
//
// APT repository: when the environment variable APT_REPO names the directory of the reprepro repository
// (e.g. /var/www/apt, set under Manage Jenkins → System → Global properties), the Publish stage adds the .deb
// to it, in the suite APT_SUITE (default "stable"). With APT_HOST (user@host) the repository is on another
// machine, reached with the SSH key of the Jenkins user. Only builds of main are published.
//
// Optional: set PARLA_CONTENT_TARGET (e.g. "root@mirranet.de:/var/lib/parla/content/") to publish course files
// after a successful build on main – the app picks them up on its next sync.
pipeline {
    agent any
    options { buildDiscarder(logRotator(numToKeepStr: '20')) }

    stages {
        stage('Build') {
            steps {
                // Installs Rust via rustup on first run if cargo is missing (needs a C compiler for SQLite).
                sh '''
                    chmod +x scripts/setup-rust.sh packaging/build-deb.sh packaging/apt/publish.sh
                    scripts/setup-rust.sh > .build-env
                    . ./.build-env
                    cargo test --locked
                    cargo build --release --locked
                    # The workspace is kept between builds, so packages of earlier builds must not be left over.
                    rm -rf dist target/debian
                    mkdir -p dist
                    # Every build gets its own package version, newer than the one before, so apt sees it as an
                    # update even while Cargo.toml keeps the same version: 0.4.0+202611142030.gitabc1234
                    VERSION="$(sed -n 's/^version *= *"\\(.*\\)"/\\1/p' Cargo.toml | head -n 1)"
                    COMMIT="$(git rev-parse --short=7 HEAD 2>/dev/null || echo local)"
                    DEB_VERSION="$VERSION+$(date -u +%Y%m%d%H%M).git$COMMIT"
                    # Debian package: dist/parla-server_<deb version>_<arch>.deb
                    DEB="$(DEB_VERSION="$DEB_VERSION" SKIP_BUILD=1 packaging/build-deb.sh)"
                    cp "$DEB" dist/
                '''
            }
            post {
                success {
                    archiveArtifacts artifacts: 'dist/*.deb, target/release/parla-server', fingerprint: true
                }
            }
        }
        stage('Publish') {
            when {
                expression { env.APT_REPO?.trim() && (env.BRANCH_NAME == null || env.BRANCH_NAME == 'main') }
            }
            steps {
                script {
                    if (env.APT_HOST?.trim()) {
                        // Repository on another machine: copy the packages and the script there and
                        // run it with that machine's reprepro and signing key.
                        sh '''
                            set -eu
                            tmp=$(ssh -o BatchMode=yes "$APT_HOST" mktemp -d)
                            scp -o BatchMode=yes -q packaging/apt/publish.sh dist/*.deb "$APT_HOST:$tmp/"
                            ssh -o BatchMode=yes "$APT_HOST" bash -s -- "$tmp" "$APT_REPO" "${APT_SUITE:-stable}" <<'EOF'
rc=0
bash "$1/publish.sh" "$2" "$3" "$1"/*.deb || rc=$?
rm -rf "$1"
exit $rc
EOF
                        '''
                    } else {
                        sh 'packaging/apt/publish.sh "$APT_REPO" "${APT_SUITE:-stable}" dist/*.deb'
                    }
                }
            }
        }
        stage('Publish content') {
            when { allOf { branch 'main'; expression { env.PARLA_CONTENT_TARGET } } }
            steps {
                sh 'rsync -av --delay-updates content/courses/*.json "$PARLA_CONTENT_TARGET"'
            }
        }
    }
}
