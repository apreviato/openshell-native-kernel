let
  upstream = import ./default.nix { };
  pkgs = import ../../../distro/nixpkgs.nix { };
  inherit (pkgs) lib;
  node = pkgs.nodejs_24;
  python = pkgs.python3;
  probes = ./openshell-probes;
  # The live and install media (tools/qemu/run.sh, builds_media): the system as
  # people use it, without the smokes' probe applications and test installer,
  # which the Desktop would list and the computer would keep in memory.
  media = builtins.pathExists ./openshell-media;
  # Installing the live system on a disk (distro/image/install.sh, with the
  # payload tools/qemu/run.sh puts in ./openshell-media).
  installer =
    if media then
      pkgs.writeShellScriptBin "openshell-native-install" ''
        export PATH=${lib.makeBinPath [ pkgs.parted pkgs.e2fsprogs pkgs.dosfstools pkgs.mtools ]}:$PATH
        ${builtins.readFile ./openshell-media/install.sh}
      ''
    else
      null;
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
        # Openbox with OpenShell's configuration (the session's window manager).
        windowManager = openbox;
        # setxkbmap: hostd's keyboard service sets the session's layout with it
        # (Settings ▸ Keyboard); xset: its lock service, when the screen turns
        # off; xrandr: its display service (Settings ▸ Display).
        tools = [
          pkgs.xvfb
          openbox
          pkgs.xdotool
          pkgs.xwd
          pkgs.setxkbmap
          pkgs.xset
          pkgs.xrandr
        ];
        # Xvfb and libglvnd find the drivers where NixOS installs them.
        mesa = pkgs.mesa;
        # Sound: the session's PulseAudio (probes/openshell-sound.pa, which
        # the launcher starts) over the kernel's ALSA devices, its pactl for
        # hostd's mixer, and ALSA's default sent to it through the pulse
        # plugin, as NixOS sets it up (without the server: the card through
        # plughw, as ALSA's dmix does not run on this kernel yet); alsa-utils
        # to check.
        sound = {
          pulseaudio = pkgs.pulseaudio;
          alsaUtils = pkgs.alsa-utils;
          asoundConf = pkgs.writeText "asound.conf" ''
            pcm_type.pulse { libs.native = ${pkgs.alsa-plugins}/lib/alsa-lib/libasound_module_pcm_pulse.so; }
            ctl_type.pulse { libs.native = ${pkgs.alsa-plugins}/lib/alsa-lib/libasound_module_ctl_pulse.so; }
            pcm.!default { type pulse fallback "plughw:0" hint { show on description "Default ALSA Output (PulseAudio)" } }
            ctl.!default { type pulse fallback "sysdefault" }
          '';
        };
        # The session's X server (probes/openshell-x.sh): Xorg with the
        # modesetting driver on the kernel's DRM device, the evdev driver,
        # and Xvfb where there is no screen.
        display = pkgs.writeShellScriptBin "openshell-x" ''
          export OPENSHELL_XORG=${pkgs.xorg-server}/bin/Xorg
          export OPENSHELL_XORG_MODULES=${pkgs.xorg-server}/lib/xorg/modules,${pkgs.xf86-input-evdev}/lib/xorg/modules
          export OPENSHELL_XORG_FONTS=${pkgs.font-misc-misc}/share/fonts/X11/misc,${pkgs.font-cursor-misc}/share/fonts/X11/misc
          export OPENSHELL_XVFB=${pkgs.xvfb}/bin/Xvfb
          ${builtins.readFile (probes + "/openshell-x.sh")}
        '';
        # Match the Desktop's CSS and the font families shipped by OpenShell Live.
        fonts = pkgs.makeFontsConf {
          fontDirectories = with pkgs; [
            # Cascadia Code and Mono only, as TrueType: the package's Nerd Font,
            # Powerline and OpenType copies add 150 MB to a system in memory.
            (pkgs.runCommand "openshell-cascadia" { } ''
              mkdir -p "$out/share/fonts/truetype"
              cp ${pkgs.cascadia-code}/share/fonts/truetype/Cascadia{Code,Mono}-*.ttf "$out/share/fonts/truetype/"
            '')
            jetbrains-mono
            liberation_ttf
            dejavu_fonts
            noto-fonts
            noto-fonts-color-emoji
          ];
          includes = [ "${pkgs.fontconfig.out}/etc/fonts/conf.d" "/etc/fonts/conf.d" ];
        };
      }
    else
      null;
  # The Linux apps profile: Xpra presents each application's windows to the
  # Desktop through its HTML5 client, as on OpenShell Live, with a GTK and a Qt
  # window for the smoke. The apps action asks for it (tools/qemu/run.sh).
  apps =
    if graphics != null && builtins.pathExists ./openshell-apps then
      rec {
        # Xpra without what the Desktop does not use and the whole system
        # would carry in memory: webcam forwarding (OpenCV, NumPy, BLAS), its
        # OpenGL client (PyOpenGL) and GStreamer's "bad" and "ugly" plugins
        # (speech synthesis, MIDI), which PulseAudio's full build brings too.
        # Xpra treats each one as optional.
        xpraPackage = (pkgs.xpra.override { pulseaudioFull = pkgs.pulseaudio; }).overrideAttrs (old: {
          propagatedBuildInputs = lib.filter (input: !(builtins.elem (input.pname or "") [ "opencv" "numpy" "pyopengl" "pyopengl-accelerate" ])) old.propagatedBuildInputs;
          buildInputs = lib.filter (input: !(builtins.elem (input.pname or "") [ "gst-plugins-bad" "gst-plugins-ugly" ])) old.buildInputs;
        });
        # The guest's X server is Xvfb. Xpra's default, Xorg with the dummy
        # driver, names a configuration file that does not exist under Nix.
        xpra = pkgs.symlinkJoin {
          name = "openshell-xpra";
          paths = [ xpraPackage ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            rm "$out/bin/xpra"
            makeWrapper ${xpraPackage}/bin/xpra "$out/bin/xpra" --add-flags \
              "--xvfb='${pkgs.xvfb}/bin/Xvfb -nolisten tcp -noreset +extension GLX +extension Composite +extension RANDR +extension RENDER -extension DOUBLE-BUFFER -auth \$XAUTHORITY -screen 0 8192x4096x24+32 -dpi 96x96'"
          '';
        };
        html5 = pkgs.xpra-html5;
        # hostd runs its desktop portal (D-Bus and GLib) and the cursor theme of
        # the apps (cursorTheme.py, which loads the X libraries) with the python3 of the PATH.
        pythonEnv = pkgs.python3.withPackages (p: [ p.dbus-python p.pygobject3 ]);
        python = pkgs.symlinkJoin {
          name = "openshell-apps-python";
          paths = [ pythonEnv ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            rm "$out/bin/python3"
            makeWrapper ${pythonEnv}/bin/python3 "$out/bin/python3" \
              --prefix GI_TYPELIB_PATH : "${lib.makeSearchPath "lib/girepository-1.0" [ pkgs.glib.out pkgs.gobject-introspection ]}" \
              --set OPENSHELL_X11_LIBRARY_PATH "${lib.makeLibraryPath [ pkgs.libx11 pkgs.libxcursor pkgs.libxfixes ]}"
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
        # openshell-*, which belong to the system itself. The GTK probe opens a
        # document type OpenShell has no viewer for, so the Desktop offers it
        # for those files ("Open with").
        entries = pkgs.runCommand "openshell-probe-applications" { } ''
          mkdir -p "$out/share/applications"
          for probe in gtk:GTK qt:Qt; do
            id="''${probe%%:*}" name="''${probe#*:}"
            printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=OpenShell $name Probe" \
              "Exec=openshell-$id-probe %f" "Categories=Utility;" > "$out/share/applications/probe-$id.desktop"
          done
          echo "MimeType=application/vnd.oasis.opendocument.text;" >> "$out/share/applications/probe-gtk.desktop"
        '';
        # dbus-run-session reads /etc/dbus-1/session.conf, which the packaged
        # configuration includes in turn: the copy leaves that include out.
        dbusSession = pkgs.runCommand "openshell-dbus-session" { } ''
          mkdir -p "$out/etc/dbus-1"
          sed '\|<include ignore_missing="yes">/etc/dbus-1/session.conf</include>|d' \
            ${pkgs.dbus}/share/dbus-1/session.conf > "$out/etc/dbus-1/session.conf"
        '';
        # cursorTheme.py draws the applications' OpenShell cursor from Adwaita's
        # (share/icons, in XDG_DATA_DIRS) and tells the toolkits through xsettingsd and xrdb.
        probeApps = lib.optionals (!media) [ gtkProbe qtProbe entries ];
        # tzdata: the time zones of Settings ▸ Date & time (/usr/share/zoneinfo);
        # xkeyboard-config: the layouts of Settings ▸ Keyboard (/usr/share/X11/xkb).
        packages = [ xpra html5 python dbusSession pkgs.dbus pkgs.adwaita-icon-theme pkgs.xsettingsd pkgs.xrdb pkgs.tzdata pkgs.xkeyboard-config ] ++ probeApps;
      }
    else
      null;
  # The Windows apps profile (OPENSHELL_WINE.md), over the Linux apps one: Wine
  # 11.0, 64-bit with WoW64 for 32-bit programs, and OpenShell's runner
  # (wine/openshell-wine.sh), which its entry opens Windows programs with: in
  # the foreground (Wine's own entry uses `start /unix`, which returns at once),
  # so the program's Xpra session lasts as long as it does.
  wine =
    if apps != null && builtins.pathExists ./openshell-wine then
      rec {
        package = pkgs.wineWow64Packages.stable;
        runner = pkgs.writeShellScriptBin "openshell-wine" (builtins.readFile ./openshell-wine/openshell-wine.sh);
        # Run a Windows program, install one into a prefix of its own (both
        # offered for .exe and .msi), and Windows apps ▸ Configure. The
        # Desktop names them by these ids (hostd/native/wine.ts).
        entries = pkgs.runCommand "openshell-wine-applications" { } ''
          mkdir -p "$out/share/applications"
          entry() {
            printf '%s\n' "[Desktop Entry]" "Type=Application" "Name=$2" "Exec=$3" "Icon=wine" \
              "NoDisplay=true" "Categories=Utility;" ''${4:+"MimeType=$4"} > "$out/share/applications/$1.desktop"
          }
          windows="application/x-ms-dos-executable;application/x-msi;"
          entry wine "Wine Windows Program Loader" "openshell-wine %f" "$windows"
          entry wine-install "Install with Wine" "openshell-wine --install %f" "$windows"
          entry wine-configure "Wine Configuration" "openshell-wine --configure %f"
        '';
        # The installer of the W4 test (wine-smoke.mjs): Wine's Notepad as a
        # program, with a Start menu shortcut and a setting (probe.wxs).
        probe = pkgs.runCommand "openshell-wine-probe" { nativeBuildInputs = [ pkgs.msitools ]; } ''
          install -m 0644 ${package}/lib/wine/x86_64-windows/notepad.exe probe.exe
          mkdir -p "$out/share/openshell"
          wixl -a x64 -o "$out/share/openshell/openshell-probe-setup.msi" ${./openshell-wine/probe.wxs}
        '';
      }
    else
      null;
  # The screen lock (lock/README.md), as on OpenShell Live: XSecureLock with
  # Live's lock screen as its auth module, which checks the password with the
  # session supervisor (openshell-authproto) and powers off through it
  # (openshell-power). hostd's lock service runs openshell-lock.
  lock =
    if apps != null && builtins.pathExists ./openshell-lock then
      rec {
        # Without its screensaver hacks and the htpasswd and pamtester helpers:
        # Apache, xscreensaver, Perl and systemd in a system that is in memory.
        xsecurelock = pkgs.xsecurelock.overrideAttrs (old: {
          buildInputs = lib.filter (input: !(builtins.elem (input.pname or "") [ "apache-httpd" "pamtester" ])) old.buildInputs;
          configureFlags = [ "--with-pam-service-name=login" "--without-xscreensaver" "--without-htpasswd" "--without-pamtester" ];
          postInstall = "";
        });
        python = pkgs.python3.withPackages (p: [ p.pygobject3 p.pycairo ]);
        # Live's modules as they are, with the paths of this system.
        screen = pkgs.stdenv.mkDerivation {
          name = "openshell-lock-screen";
          src = ./openshell-lock;
          nativeBuildInputs = [ pkgs.wrapGAppsHook3 pkgs.gobject-introspection ];
          buildInputs = [ pkgs.gtk3 python ];
          installPhase = ''
            modules="$out/lib/openshell-lock"
            install -Dm0644 -t "$modules" lock_screen.py lock_view.py lock_auth.py lock_settings.py
            # The session user's home is the Desktop's ($HOME, openshell-lock),
            # not the one in /etc/passwd: Settings ▸ Lock screen and the photo are there.
            substituteInPlace "$modules/lock_screen.py" \
              --replace-fail "'/usr/local/lib/openshell'" "'$modules'" \
              --replace-fail "Path('/usr/bin/xsecurelock')" "Path('${xsecurelock}/bin/xsecurelock')" \
              --replace-fail "'/usr/bin/setxkbmap'" "'${pkgs.setxkbmap}/bin/setxkbmap'" \
              --replace-fail "'/usr/bin/systemctl'" "'${graphics.launcher}/bin/openshell-power'" \
              --replace-fail "account.pw_dir" "(os.environ.get('HOME') or account.pw_dir)"
            substituteInPlace "$modules/lock_auth.py" \
              --replace-fail "'/usr/libexec/xsecurelock/authproto_pam'" "'${graphics.launcher}/bin/openshell-authproto'"
            # XSecureLock's auth module, auth_openshell as on Live: XSecureLock
            # takes only auth_* executables.
            mkdir -p "$out/bin"
            printf '#!%s\nexec %s -I %s\n' "${pkgs.runtimeShell}" "${python}/bin/python3" "$modules/lock_screen.py" > "$out/bin/auth_openshell"
            chmod 0755 "$out/bin/auth_openshell"
          '';
        };
        ready = pkgs.writeShellScript "openshell-lock-ready" (builtins.readFile ./openshell-lock/lock-ready.sh);
        # Num Lock remembered across restarts, as on Live (numlock.py, Live's),
        # started with the window manager (openshell-wm.sh).
        numlock = pkgs.runCommand "openshell-numlock" { } ''
          install -Dm0644 ${./openshell-lock/numlock.py} "$out/lib/numlock.py"
          substituteInPlace "$out/lib/numlock.py" \
            --replace-fail "C.CDLL('libX11.so.6')" "C.CDLL('${pkgs.libx11}/lib/libX11.so.6')"
        '';
        windowManager = pkgs.runCommand "openshell-wm" { } ''
          mkdir -p "$out/bin"
          substitute ${./openshell-lock/openshell-wm.sh} "$out/bin/openshell-wm" \
            --replace-fail @python@ ${pkgs.python3}/bin/python3 \
            --replace-fail @numlock@ ${numlock}/lib/numlock.py \
            --replace-fail @openbox@ ${graphics.windowManager}/bin/openbox
          chmod 0755 "$out/bin/openshell-wm"
        '';
        command = pkgs.runCommand "openshell-lock" { } ''
          mkdir -p "$out/bin"
          substitute ${./openshell-lock/lock.sh} "$out/bin/openshell-lock" \
            --replace-fail @env@ ${pkgs.coreutils}/bin/env \
            --replace-fail @path@ /opt/openshell-compat/bin:/bin:/usr/bin \
            --replace-fail @cursors@ ${pkgs.adwaita-icon-theme}/share/icons \
            --replace-fail @auth@ ${screen}/bin/auth_openshell \
            --replace-fail @authproto@ ${graphics.launcher}/bin/openshell-authproto \
            --replace-fail @saver@ ${xsecurelock}/libexec/xsecurelock/saver_blank \
            --replace-fail @xsecurelock@ ${xsecurelock}/bin/xsecurelock \
            --replace-fail @ready@ ${ready}
          chmod 0755 "$out/bin/openshell-lock"
        '';
      }
    else
      null;
  # The applications the Desktop finds where Debian has them (/usr/share/applications).
  applications = pkgs.symlinkJoin {
    name = "openshell-applications";
    paths = lib.optional (apps != null && !media) apps.entries ++ lib.optional (wine != null) wine.entries;
  };
  # The Desktop Terminal integrates with bash, as on OpenShell Live.
  shell = if bundle != null then pkgs.bashInteractive else null;
  # The Desktop's local models (Models ▸ On-Device, Settings ▸ Local models):
  # llama.cpp's server at the commit OpenShell Live builds
  # (apps/desktop/scripts/install-native.sh, b11960, the first that loads
  # LFM3), whose version nixpkgs does not have yet. It runs on the CPU, as
  # Native has no GPU driver: one library per instruction set (SSE4.2 to
  # AVX-512 and AMX), the best chosen when it starts. hostd finds it next to
  # its own folder, in /opt/openshell/llama/bin, as on Live; the libraries
  # sit beside it ($ORIGIN), where llama.cpp looks for its CPU backends.
  llama =
    if bundle != null then
      pkgs.stdenv.mkDerivation {
        pname = "openshell-llama-server";
        version = "b11960";
        src = pkgs.fetchurl {
          name = "llama.cpp-8642541b.tar.gz";
          url = "https://codeload.github.com/ggml-org/llama.cpp/tar.gz/8642541b255bfb5dc39cd351a178be09013a55d6";
          sha256 = "1688a167a8edd9499cdd0c19b8bd044f0fa184b466981977bbf2222d884953a4";
        };
        nativeBuildInputs = [ pkgs.cmake pkgs.ninja pkgs.patchelf ];
        cmakeFlags = [
          "-DGGML_NATIVE=OFF"
          "-DGGML_BACKEND_DL=ON"
          "-DGGML_CPU_ALL_VARIANTS=ON"
          "-DLLAMA_BUILD_TESTS=OFF"
          "-DLLAMA_BUILD_EXAMPLES=OFF"
          "-DLLAMA_BUILD_SERVER=ON"
          "-DLLAMA_OPENSSL=OFF"
          "-DLLAMA_CURL=OFF"
          "-DLLAMA_USE_PREBUILT_UI=OFF"
        ];
        ninjaFlags = [ "llama-server" ];
        installPhase = ''
          runHook preInstall
          mkdir -p "$out/bin"
          install -m 0755 bin/llama-server "$out/bin/"
          find bin -maxdepth 1 -name '*.so*' -exec cp -P '{}' "$out/bin/" ';'
          # The libraries beside the server, and the store's (libstdc++,
          # libgomp), never the build folder.
          for file in "$out"/bin/*; do
            [ -L "$file" ] && continue
            rpath=$(patchelf --print-rpath "$file" | tr ':' '\n' | grep -v -e '^$' -e "^$NIX_BUILD_TOP" | paste -sd: -)
            patchelf --set-rpath "\$ORIGIN''${rpath:+:$rpath}" "$file"
          done
          install -m 0644 ../LICENSE "$out/LICENSE"
          echo 8642541b255bfb5dc39cd351a178be09013a55d6 > "$out/VERSION"
          runHook postInstall
        '';
        doInstallCheck = true;
        installCheckPhase = ''
          "$out/bin/llama-server" --version
        '';
      }
    else
      null;
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
  # The installed system's start from its copy on the data partition
  # (probes/disk-root.sh): a few megabytes GRUB loads in place of the whole
  # system, once a slot's first boot has copied it there (installed-session.sh).
  # Nothing of the copy runs before it is the root: the image's BusyBox comes
  # along with the loader and libraries it names, at their store paths.
  bootRoot = pkgs.runCommand "openshell-boot-root.cpio" { nativeBuildInputs = [ pkgs.cpio pkgs.patchelf ]; } ''
    mkdir -p root/bin root/dev root/proc root/sys root/data root/newroot
    cp ${pkgs.busybox}/bin/busybox root/bin/busybox
    interpreter=$(patchelf --print-interpreter root/bin/busybox)
    libraries=$(patchelf --print-rpath root/bin/busybox)
    mkdir -p "root$(dirname "$interpreter")"
    cp -L "$interpreter" "root$interpreter"
    for library in $(patchelf --print-needed root/bin/busybox); do
      for folder in $(echo "$libraries" | tr : ' '); do
        if [ -e "$folder/$library" ]; then
          mkdir -p "root$folder"
          cp -L "$folder/$library" "root$folder/$library"
          break
        fi
      done
      found=$(find root -name "$library" | head -n1)
      [ -n "$found" ] || { echo "busybox needs $library" >&2; exit 1; }
    done
    install -m 0755 ${probes}/disk-root.sh root/init
    cd root
    find . -mindepth 1 | sed 's|^\./||' | LC_ALL=C sort | cpio --quiet -o -H newc -R 0:0 > "$out"
  '';
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
        graphics.fonts
        graphics.display
      ]
      # Software OpenGL (Mesa with LLVM, 800 MB) for WebGL and the apps' GLX.
      # The media, which run from memory, leave it out: the CEF host then uses
      # its own SwiftShader for WebGL.
      ++ lib.optional (!media) graphics.mesa
      ++ graphics.tools
      ++ [ graphics.sound.pulseaudio graphics.sound.alsaUtils graphics.sound.asoundConf ]
    )
    ++ lib.optionals (apps != null) apps.packages
    ++ lib.optionals (wine != null) ([ wine.package wine.runner ] ++ lib.optional (!media) wine.probe)
    ++ lib.optionals (lock != null) [ lock.command lock.windowManager ]
    # /usr/share/applications links to it.
    ++ lib.optional (apps != null) applications
    ++ lib.optional (shell != null) shell
    ++ lib.optional (llama != null) llama
    ++ lib.optional (ab != null) ab
    ++ lib.optional (installer != null) installer
  );
  extended = pkgs.runCommand "openshell-native-compat-root" { } ''
    mkdir -p "$out"
    cp -R ${upstream.initramfs}/. "$out/"
    chmod u+w "$out" "$out/nix/store" "$out/opt" "$out/test"
    # The DNS servers the kernel's DHCP client got (/proc/net/pnp, in
    # resolv.conf's format), as Linux's IP autoconfiguration offers them.
    chmod u+w "$out/etc"
    ln -sfn /proc/net/pnp "$out/etc/resolv.conf"
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
      # One boot of the disk image test (distro/image/boot-test.sh).
      install -m 0644 ${probes}/image-boot.sh "$out/opt/openshell-compat/image-boot.sh"
      install -m 0644 ${probes}/installed-session.sh "$out/opt/openshell-compat/installed-session.sh"
      install -m 0644 ${probes}/installed-storage.sh "$out/opt/openshell-compat/installed-storage.sh"
      install -m 0644 ${bootRoot} "$out/opt/openshell-compat/boot-root.cpio"
    ''}
    install -m 0644 ${probes}/hostd-smoke.mjs ${probes}/unprivileged.py ${probes}/node.mjs ${probes}/python.py ${probes}/inotify.py ${probes}/namespaces.py "$out/opt/openshell-compat/"
    ${lib.optionalString (bundle != null) ''
      mkdir -p "$out/opt/openshell"
      cp -R ${bundle}/. "$out/opt/openshell/"
      chmod u+w "$out/bin"
      ln -s ${shell}/bin/bash "$out/bin/bash"
      # llama.cpp built for this system: a runtime a Debian bundle carries
      # would look for Debian's libraries.
      chmod -R u+w "$out/opt/openshell"
      rm -rf "$out/opt/openshell/llama"
      mkdir -p "$out/opt/openshell/llama"
      ln -s ${llama}/bin "$out/opt/openshell/llama/bin"
      ln -s ${llama}/LICENSE ${llama}/VERSION "$out/opt/openshell/llama/"
    ''}
    ${lib.optionalString (graphics != null) ''
      for tool in ${lib.concatMapStringsSep " " (tool: "${tool}/bin") graphics.tools}; do
        for program in Xvfb openbox xdotool xwd setxkbmap xset xrandr; do
          if [ -e "$tool/$program" ]; then
            ln -s "$tool/$program" "$out/opt/openshell-compat/bin/$program"
          fi
        done
      done
      ln -s ${graphics.cefHost}/runtime "$out/opt/openshell-cef"
      ln -s ${graphics.launcher}/bin/openshell-shell "$out/opt/openshell-compat/bin/openshell-shell"
      ln -s ${graphics.launcher}/bin/openshell-session "$out/opt/openshell-compat/bin/openshell-session"
      ln -s ${graphics.display}/bin/openshell-x "$out/opt/openshell-compat/bin/openshell-x"
      ${lib.optionalString (!media) ''
        mkdir -p "$out/run"
        ln -s ${graphics.mesa} "$out/run/opengl-driver"
      ''}
      chmod u+w "$out/etc"
      mkdir -p "$out/etc/fonts"
      chmod u+w "$out/etc/fonts"
      ln -sfn ${graphics.fonts} "$out/etc/fonts/fonts.conf"
      # Sound (graphics.sound): the server, its configuration, ALSA's default.
      for program in ${graphics.sound.pulseaudio}/bin/pulseaudio ${graphics.sound.pulseaudio}/bin/pactl \
          ${graphics.sound.alsaUtils}/bin/aplay ${graphics.sound.alsaUtils}/bin/arecord ${graphics.sound.alsaUtils}/bin/amixer; do
        ln -s "$program" "$out/opt/openshell-compat/bin/"
      done
      install -m 0644 ${probes}/openshell-sound.pa "$out/opt/openshell-compat/sound.pa"
      ln -sfn ${graphics.sound.asoundConf} "$out/etc/asound.conf"
      ln -s /etc/fonts/fonts.conf "$out/opt/openshell-compat/fonts.conf"
      ln -s ${pkgs.openbox}/etc/xdg "$out/opt/openshell-compat/xdg"
      install -m 0644 ${probes}/cef-smoke.mjs ${probes}/shell-smoke.mjs ${probes}/session-smoke.mjs ${probes}/linux-sandbox.mjs "$out/opt/openshell-compat/"
    ''}
    ${lib.optionalString (apps != null) ''
      # Where hostd looks for them, as on Debian.
      ln -sf ${apps.python}/bin/python3 "$out/opt/openshell-compat/bin/python3"
      for program in ${apps.xpra}/bin/xpra ${pkgs.dbus}/bin/dbus-run-session ${pkgs.dbus}/bin/dbus-daemon \
          ${lib.optionalString (!media) "${apps.gtkProbe}/bin/openshell-gtk-probe ${apps.qtProbe}/bin/openshell-qt-probe"} \
          ${pkgs.xsettingsd}/bin/xsettingsd ${pkgs.xrdb}/bin/xrdb; do
        ln -s "$program" "$out/opt/openshell-compat/bin/"
      done
      # The upstream root comes from the store, read-only.
      chmod u+w "$out" "$out/etc"
      mkdir -p "$out/usr"
      chmod u+w "$out/usr"
      if [ -d "$out/usr/share" ]; then chmod u+w "$out/usr/share"; fi
      mkdir -p "$out/etc/dbus-1" "$out/usr/share/xpra"
      ln -s ${apps.dbusSession}/etc/dbus-1/session.conf "$out/etc/dbus-1/session.conf"
      ln -s ${apps.html5}/share/xpra/www "$out/usr/share/xpra/www"
      ln -s ${applications}/share/applications "$out/usr/share/applications"
      # The cursor theme of the applications draws from Adwaita, where Debian has it.
      ln -s ${pkgs.adwaita-icon-theme}/share/icons "$out/usr/share/icons"
      # The time zones, where /etc/localtime points (the supervisor's settings.rs).
      ln -s ${pkgs.tzdata}/share/zoneinfo "$out/usr/share/zoneinfo"
      # The keyboard layouts and their names (Settings ▸ Keyboard reads
      # rules/base.lst; setxkbmap and X compile from the same data).
      mkdir -p "$out/usr/share/X11"
      ln -s ${pkgs.xkeyboard-config}/share/X11/xkb "$out/usr/share/X11/xkb"
      install -m 0644 ${probes}/apps-smoke.mjs "$out/opt/openshell-compat/"
    ''}
    ${lib.optionalString (wine != null) ''
      for program in wine wineboot wineserver; do
        ln -s "${wine.package}/bin/$program" "$out/opt/openshell-compat/bin/"
      done
      ln -s ${wine.runner}/bin/openshell-wine "$out/opt/openshell-compat/bin/"
      ${lib.optionalString (!media) ''ln -s ${wine.probe}/share/openshell/openshell-probe-setup.msi "$out/opt/openshell-compat/openshell-probe-setup.msi"''}
      install -m 0644 ${probes}/wine-smoke.mjs "$out/opt/openshell-compat/"
    ''}
    ${lib.optionalString (lock != null) ''
      ln -s ${lock.command}/bin/openshell-lock "$out/opt/openshell-compat/bin/"
      ln -s ${lock.windowManager}/bin/openshell-wm "$out/opt/openshell-compat/bin/"
      ln -s ${graphics.launcher}/bin/openshell-power "$out/opt/openshell-compat/bin/"
    ''}
    ${lib.optionalString media ''
      mkdir -p "$out/opt/openshell-media"
      cp -R ${./openshell-media}/. "$out/opt/openshell-media/"
      ln -s ${installer}/bin/openshell-native-install "$out/opt/openshell-compat/bin/openshell-native-install"
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
  # The Windows apps profile, for the Linux control of the builder.
  wineTools = if wine != null then pkgs.symlinkJoin {
    name = "openshell-wine-tools";
    paths = [ wine.package wine.runner wine.entries wine.probe ];
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
