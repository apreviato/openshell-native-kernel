let
  upstream = import ./default.nix { };
  pkgs = import ../../../distro/nixpkgs.nix { };
  inherit (pkgs) lib;
  node = pkgs.nodejs_24;
  python = pkgs.python3;
  probes = ./openshell-probes;
  # The OpenShell Desktop bundle, when the build provides one.
  bundle = if builtins.pathExists ./openshell-bundle then ./openshell-bundle else null;
  seccomp = pkgs.runCommandCC "openshell-seccomp-probe" { } ''
    mkdir -p "$out/bin"
    $CC -O2 -Wall -Wextra -Werror -pthread -o "$out/bin/seccomp-probe" ${probes}/seccomp.c
    $CC -O2 -Wall -Wextra -Werror -pthread -o "$out/bin/chromium-userns" ${probes}/chromium-userns.c
  '';
  # The graphics profile: an X server, a window manager, the X11 tools that the CEF
  # smoke drives, Mesa for OpenGL in software and the Rust launcher with the CEF
  # host, when the build provides the shell sources.
  graphics =
    if builtins.pathExists ./openshell-shell then
      let
        shell = import ./openshell-shell/default.nix { inherit pkgs; };
      in
      {
        inherit (shell) cefHost;
        launcher = shell.shell;
        tools = [
          pkgs.xvfb
          pkgs.openbox
          pkgs.xdotool
          pkgs.xwd
        ];
        # Xvfb and libglvnd find the drivers where NixOS installs them.
        mesa = pkgs.mesa;
        fonts = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
      }
    else
      null;
  # The Desktop Terminal integrates with bash, as on OpenShell Live.
  shell = if bundle != null then pkgs.bashInteractive else null;
  # The A/B release transaction, exercised on the guest's VirtIO disk.
  ab =
    if builtins.pathExists ./openshell-ab then
      pkgs.rustPlatform.buildRustPackage {
        pname = "openshell-ab";
        version = "0.1.0";
        src = ./openshell-ab;
        cargoLock.lockFile = ./openshell-ab/Cargo.lock;
      }
    else
      null;
  # A throwaway authority and server certificate for the TLS probe.
  tls = pkgs.runCommand "openshell-probe-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir -p "$out"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
      -keyout "$out/key.pem" -out "$out/cert.pem" -days 3650 -subj /CN=localhost \
      -addext subjectAltName=DNS:localhost,IP:127.0.0.1
  '';
  runtimeClosure = pkgs.writeClosure (
    [
      node
      python
      seccomp
      tls
    ]
    ++ lib.optionals (graphics != null) (
      [
        graphics.cefHost
        graphics.launcher
        graphics.mesa
        graphics.fonts
      ]
      ++ graphics.tools
    )
    ++ lib.optional (shell != null) shell
    ++ lib.optional (ab != null) ab
  );
  extended = pkgs.runCommand "openshell-native-compat-root" { } ''
    mkdir -p "$out"
    cp -R ${upstream.initramfs}/. "$out/"
    chmod u+w "$out" "$out/nix/store" "$out/opt" "$out/test"
    while IFS= read -r dependency; do
      name="$(basename "$dependency")"
      if [ ! -e "$out/nix/store/$name" ]; then
        cp -R "$dependency" "$out/nix/store/"
      fi
    done < ${runtimeClosure}
    mkdir -p "$out/opt/openshell-compat/bin"
    # hostd finds Python on the PATH for its PTY backend.
    ln -s ${node}/bin/node "$out/opt/openshell-compat/bin/node"
    ln -s ${python}/bin/python3 "$out/opt/openshell-compat/bin/python3"
    ln -s ${node}/bin/node "$out/opt/openshell-compat/node"
    ln -s ${python}/bin/python3 "$out/opt/openshell-compat/python"
    ln -s ${seccomp}/bin/seccomp-probe "$out/opt/openshell-compat/seccomp-probe"
    ln -s ${seccomp}/bin/chromium-userns "$out/opt/openshell-compat/chromium-userns"
    ln -s ${tls} "$out/opt/openshell-compat/tls"
    ${lib.optionalString (ab != null) ''
      ln -s ${ab}/bin/openshell-ab "$out/opt/openshell-compat/openshell-ab"
      install -m 0644 ${./openshell-ab/smoke.sh} "$out/opt/openshell-compat/ab-smoke.sh"
    ''}
    install -m 0644 ${probes}/hostd-smoke.mjs ${probes}/unprivileged.py ${probes}/node.mjs ${probes}/python.py ${probes}/inotify.py ${probes}/namespaces.py "$out/opt/openshell-compat/"
    ${lib.optionalString (bundle != null) ''
      mkdir -p "$out/opt/openshell"
      cp -R ${bundle}/. "$out/opt/openshell/"
      chmod u+w "$out/bin"
      ln -s ${shell}/bin/bash "$out/bin/bash"
    ''}
    ${lib.optionalString (graphics != null) ''
      for tool in ${lib.concatMapStringsSep " " (tool: "${tool}/bin") graphics.tools}; do
        for program in Xvfb openbox xdotool xwd; do
          if [ -e "$tool/$program" ]; then
            ln -s "$tool/$program" "$out/opt/openshell-compat/bin/$program"
          fi
        done
      done
      ln -s ${graphics.cefHost}/runtime "$out/opt/openshell-cef"
      ln -s ${graphics.launcher}/bin/openshell-shell "$out/opt/openshell-compat/bin/openshell-shell"
      ln -s ${graphics.launcher}/bin/openshell-session "$out/opt/openshell-compat/bin/openshell-session"
      mkdir -p "$out/run"
      ln -s ${graphics.mesa} "$out/run/opengl-driver"
      ln -s ${graphics.fonts} "$out/opt/openshell-compat/fonts.conf"
      ln -s ${pkgs.openbox}/etc/xdg "$out/opt/openshell-compat/xdg"
      install -m 0644 ${probes}/cef-smoke.mjs ${probes}/shell-smoke.mjs ${probes}/session-smoke.mjs ${probes}/linux-sandbox.mjs "$out/opt/openshell-compat/"
    ''}
    rm "$out/test/boot_hello.sh"
    install -m 0755 ${probes}/compat-boot.sh "$out/test/boot_hello.sh"
  '';
in
{
  inherit node python seccomp tls ab;
  # The graphics profile, for the Linux control of the builder.
  cefHost = graphics.cefHost or null;
  launcher = graphics.launcher or null;
  mesa = graphics.mesa or null;
  fonts = graphics.fonts or null;
  graphicsTools = if graphics != null then pkgs.symlinkJoin {
    name = "openshell-graphics-tools";
    paths = graphics.tools;
  } else null;
  image = pkgs.callPackage ./initramfs-image.nix {
    initramfs = extended;
    compressed = true;
  };
}
