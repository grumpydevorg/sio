# Build sio from a positively selected source set. The package also installs
# the agent skill the binary embeds at share/agent-skills/sio/SKILL.md, so a
# consumer can link agents' skill directories at it.
{
  lib,
  pkg-config,
  rustPlatform,
  stdenv,
  udev,
}:

let
  fs = lib.fileset;
  cargoToml = lib.importTOML ./Cargo.toml;

  cargoSource = fs.unions [
    ./Cargo.toml
    ./Cargo.lock
    ./LICENSE-APACHE
    ./LICENSE-MIT
    ./src
    ./skills/sio/SKILL.md
    ./tests
  ];
in
rustPlatform.buildRustPackage {
  pname = "sio";
  inherit (cargoToml.package) version;

  src = fs.toSource {
    root = ./.;
    fileset = cargoSource;
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [ pkg-config ];
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ udev ];

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    "$out/bin/sio" --version | grep -Fx 'sio ${cargoToml.package.version}'
    "$out/bin/sio" skill --check "$out/share/agent-skills/sio/SKILL.md"
    "$out/bin/sio" transact --loopback --port loop:// --text test --expect test --json \
      | grep -F '"matched":true'
    runHook postInstallCheck
  '';

  postInstall = ''
    install -Dm644 skills/sio/SKILL.md "$out/share/agent-skills/sio/SKILL.md"
  '';

  meta = {
    inherit (cargoToml.package) description;
    homepage = cargoToml.package.repository;
    mainProgram = "sio";
    license = with lib.licenses; [
      mit
      asl20
    ];
    platforms = lib.platforms.unix;
  };
}
