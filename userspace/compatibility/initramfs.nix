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
        openbox = pkgs.symlinkJoin {
          name = "openshell-openbox";
          paths = [ pkgs.openbox ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            # OpenShell handles window switching, closing and Super shortcuts.
            config="$out/etc/xdg/openbox/openshell.xml"
            sed -E '/<keybind key="(A-(Escape|F4|Tab|S-Tab)|W-[^"]+)">/,/<\/keybind>/d' \
              ${pkgs.openbox}/etc/xdg/openbox/rc.xml > "$config"
            rm "$out/bin/openbox"
            makeWrapper ${pkgs.openbox}/bin/openbox "$out/bin/openbox" --add-flags "--config-file $config"
          '';
        };
      in
      {
        inherit (shell) cefHost;
        launcher = shell.shell;
        tools = [
          pkgs.xvfb
          openbox
          pkgs.xdotool
          pkgs.xwd
        ];
        # Xvfb and libglvnd find the drivers where NixOS installs them.
        mesa = pkgs.mesa;
        fonts = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
      }
    else
      null;
  # The Linux apps profile: Xpra presents each application's windows to the
  # Desktop through its HTML5 client, as on OpenShell Live, with a GTK and a Qt
  # window for the smoke. The apps action asks for it (tools/qemu/run.sh).
  apps =
    if graphics != null && builtins.pathExists ./openshell-apps then
      rec {
        # The guest's X server is Xvfb. Xpra's default, Xorg with the dummy
        # driver, names a configuration file that does not exist under Nix.
        xpra = pkgs.symlinkJoin {
          name = "openshell-xpra";
          paths = [ pkgs.xpra ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            rm "$out/bin/xpra"
            makeWrapper ${pkgs.xpra}/bin/xpra "$out/bin/xpra" --add-flags \
              "--xvfb='${pkgs.xvfb}/bin/Xvfb -nolisten tcp -noreset +extension GLX +extension Composite +extension RANDR +extension RENDER -extension DOUBLE-BUFFER -auth \$XAUTHORITY -screen 0 8192x4096x24+32 -dpi 96x96'"
          '';
        };
        html5 = pkgs.xpra-html5;
        # hostd runs its desktop portal (D-Bus and GLib) with the python3 of the PATH.
        pythonEnv = pkgs.python3.withPackages (p: [ p.dbus-python p.pygobject3 ]);
        python = pkgs.symlinkJoin {
          name = "openshell-apps-python";
          paths = [ pythonEnv ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            rm "$out/bin/python3"
            makeWrapper ${pythonEnv}/bin/python3 "$out/bin/python3" \
              --prefix GI_TYPELIB_PATH : "${lib.makeSearchPath "lib/girepository-1.0" [ pkgs.glib.out pkgs.gobject-introspection ]}"
          '';
        };
        gtkProbe = pkgs.stdenv.mkDerivation {
          name = "openshell-gtk-probe";
          dontUnpack = true;
          nativeBuildInputs = [ pkgs.wrapGAppsHook3 pkgs.gobject-introspection ];
          buildInputs = [ pkgs.gtk3 (pkgs.python3.withPackages (p: [ p.pygobject3 ])) ];
          installPhase = ''
            install -Dm755 ${probes}/gtk-probe.py "$out/bin/openshell-gtk-probe"
            patchShebangs "$out/bin"
          '';
        };
        qtProbe = pkgs.stdenv.mkDerivation {
          name = "openshell-qt-probe";
          dontUnpack = true;
          nativeBuildInputs = [ pkgs.pkg-config pkgs.qt6.wrapQtAppsHook ];
          buildInputs = [ pkgs.qt6.qtbase ];
          buildPhase = ''
            $CXX -O2 -Wall -Wextra -Werror -fPIC -std=c++17 ${probes}/qt-probe.cpp -o openshell-qt-probe \
              $(pkg-config --cflags --libs Qt6Widgets)
          '';
          installPhase = ''
            install -Dm755 openshell-qt-probe "$out/bin/openshell-qt-probe"
          '';
        };
        # The applications as the Desktop lists them. hostd hides entries named
        # openshell-*, which belong to the system itself.
        entries = pkgs.runCommand "openshell-probe-applications" { } ''
          mkdir -p "$out/share/applications"
          for probe in gtk:GTK qt:Qt; do
            id="''${probe%%:*}" name="''${probe#*:}"
            printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=OpenShell $name Probe" \
              "Exec=openshell-$id-probe" "Categories=Utility;" > "$out/share/applications/probe-$id.desktop"
          done
        '';
        # dbus-run-session reads /etc/dbus-1/session.conf, which the packaged
        # configuration includes in turn: the copy leaves that include out.
        dbusSession = pkgs.runCommand "openshell-dbus-session" { } ''
          mkdir -p "$out/etc/dbus-1"
          sed '\|<include ignore_missing="yes">/etc/dbus-1/session.conf</include>|d' \
            ${pkgs.dbus}/share/dbus-1/session.conf > "$out/etc/dbus-1/session.conf"
        '';
        packages = [ xpra html5 python gtkProbe qtProbe entries dbusSession pkgs.dbus ];
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
    ++ lib.optionals (apps != null) apps.packages
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
    ${lib.optionalString (apps != null) ''
      # Where hostd looks for them, as on Debian.
      ln -sf ${apps.python}/bin/python3 "$out/opt/openshell-compat/bin/python3"
      for program in ${apps.xpra}/bin/xpra ${pkgs.dbus}/bin/dbus-run-session ${pkgs.dbus}/bin/dbus-daemon \
          ${apps.gtkProbe}/bin/openshell-gtk-probe ${apps.qtProbe}/bin/openshell-qt-probe; do
        ln -s "$program" "$out/opt/openshell-compat/bin/"
      done
      chmod u+w "$out" "$out/etc"
      mkdir -p "$out/etc/dbus-1" "$out/usr/share/xpra"
      ln -s ${apps.dbusSession}/etc/dbus-1/session.conf "$out/etc/dbus-1/session.conf"
      ln -s ${apps.html5}/share/xpra/www "$out/usr/share/xpra/www"
      ln -s ${apps.entries}/share/applications "$out/usr/share/applications"
      install -m 0644 ${probes}/apps-smoke.mjs "$out/opt/openshell-compat/"
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
  # The Linux apps profile, for the Linux control of the builder.
  appsTools = if apps != null then pkgs.symlinkJoin {
    name = "openshell-apps-tools";
    paths = apps.packages;
  } else null;
  image = pkgs.callPackage ./initramfs-image.nix {
    initramfs = extended;
    compressed = true;
  };
}
