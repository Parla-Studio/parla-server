// Builds the Parla server (Rust binary + Debian package).
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
                    chmod +x scripts/setup-rust.sh packaging/build-deb.sh
                    scripts/setup-rust.sh > .build-env
                    . ./.build-env
                    cargo test --locked
                    cargo build --release --locked
                    # Debian package: target/debian/parla-server_<version>_<arch>.deb
                    SKIP_BUILD=1 packaging/build-deb.sh
                '''
            }
            post {
                success {
                    archiveArtifacts artifacts: 'target/debian/*.deb, target/release/parla-server', fingerprint: true
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
