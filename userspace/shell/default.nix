# The Rust shell and the CEF host, built against the CEF of the pinned nixpkgs.
#
# The CEF host runtime directory holds the executable next to libcef.so and the
# CEF resources, as the CEF SDK expects. libcef.so keeps the RUNPATH that nixpkgs
# gave it, so its library closure comes from the same nixpkgs as the guest.
{ pkgs }:
let
  inherit (pkgs) lib;
  cef = pkgs.cef-binary;

  src = lib.cleanSourceWith {
    src = ./.;
    filter = path: _type: builtins.baseNameOf path != "target";
  };

  shell = pkgs.stdenv.mkDerivation {
    pname = "openshell-shell";
    version = "0.1.0";
    inherit src;
    nativeBuildInputs = [
      pkgs.cargo
      pkgs.rustc
    ];
    buildPhase = ''
      runHook preBuild
      export CARGO_HOME="$TMPDIR/cargo"
      cargo build --offline --locked --release
      runHook postBuild
    '';
    doCheck = true;
    checkPhase = ''
      runHook preCheck
      cargo test --offline --locked --release
      runHook postCheck
    '';
    installPhase = ''
      runHook preInstall
      install -Dm0644 target/release/libopenshell_shell.a "$out/lib/libopenshell_shell.a"
      install -Dm0755 target/release/openshell-shell "$out/bin/openshell-shell"
      install -Dm0755 target/release/openshell-session "$out/bin/openshell-session"
      install -Dm0755 target/release/openshell-authproto "$out/bin/openshell-authproto"
      install -Dm0755 target/release/openshell-power "$out/bin/openshell-power"
      runHook postInstall
    '';
  };

  cefHost = pkgs.stdenv.mkDerivation {
    pname = "openshell-cef-host";
    inherit (cef) version;
    src = "${src}/cef";
    nativeBuildInputs = [ pkgs.cmake ];
    buildInputs = [ pkgs.libx11 pkgs.libxext pkgs.libxfixes pkgs.libxscrnsaver ];
    cmakeFlags = [
      "-DCEF_ROOT=${cef}"
      "-DCMAKE_BUILD_TYPE=Release"
      "-DUSE_SANDBOX=ON"
      "-DOPENSHELL_CEF_POLICY_LIBRARY=${shell}/lib/libopenshell_shell.a"
    ];
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/runtime"
      cp -R runtime/. "$out/runtime/"
      chmod -R u+w "$out/runtime"
      # The SDK ships libcef.so with its debug information, about 1.3 GB.
      # --strip-unneeded keeps the dynamic symbol table and the RUNPATH.
      strip --strip-unneeded "$out/runtime/libcef.so"
      # The GPU process loads ANGLE, SwiftShader and the Vulkan loader with dlopen.
      # Their own dependencies do not use the RUNPATH of libcef.so.
      for library in libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1; do
        patchelf --add-rpath ${
          lib.makeLibraryPath [
            pkgs.libglvnd
            pkgs.libx11
            pkgs.libxext
            pkgs.libxcb
          ]
        } "$out/runtime/$library"
      done
      # The setuid helper is not used: the sandbox runs in user namespaces.
      rm -f "$out/runtime/chrome-sandbox"
      # The host loads libcef.so from its own folder ($ORIGIN). The SDK's folder,
      # which the build left in its RUNPATH, would bring the whole SDK (1.3 GB)
      # into every image that holds the host.
      rpath=$(patchelf --print-rpath "$out/runtime/openshell-cef-host" | tr ':' '\n' | grep -v '^${cef}' | paste -sd: -)
      patchelf --set-rpath "$rpath" "$out/runtime/openshell-cef-host"
      if grep -rq '${builtins.baseNameOf cef}' "$out"; then
        echo "The CEF host still refers to the SDK" >&2
        exit 1
      fi
      grep -m1 '^#define CEF_VERSION ' ${cef}/include/cef_version.h > "$out/cef-version.txt"
      runHook postInstall
    '';
  };
in
{
  inherit cef shell cefHost;
}
