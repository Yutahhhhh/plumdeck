"""Build the pinned real Mixxx host from an MSVC developer command prompt."""
from pathlib import Path
import hashlib
import os
import re
import shutil
import subprocess
import sys
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
DEPS = ROOT / "build-deps"
MIXXX = "3ebac449e7e5fe2a0186596657696e87ce8b0e56"
VCPKG = "3d33b7f7a8121da6e4fa80b35618fc8e960be686"
LDC = "20bf658a60881854984f8ffb1586a4722bf590ec"


def run(*args, cwd=None):
    subprocess.run([str(a) for a in args], cwd=cwd, check=True)


def fetch(url, revision, destination):
    if not (destination / ".git").exists():
        run("git", "init", destination)
        run("git", "-C", destination, "remote", "add", "origin", url)
        run("git", "-C", destination, "fetch", "--depth", "1", "origin", revision)
        run("git", "-C", destination, "checkout", "--detach", "FETCH_HEAD")
    actual = subprocess.check_output(["git", "-C", str(destination), "rev-parse", "HEAD"], text=True).strip()
    if actual != revision:
        raise RuntimeError(f"Unexpected dependency revision: {destination}")


def download(url, destination, digest=None):
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not destination.exists():
        temporary = destination.with_suffix(destination.suffix + ".download")
        run("curl.exe", "--fail", "--location", "--retry", "3", "--connect-timeout", "30", "--max-time", "1800", "--speed-time", "120", "--speed-limit", "1024", "--output", temporary, url)
        temporary.replace(destination)
    if digest and hashlib.sha256(destination.read_bytes()).hexdigest() != digest:
        raise RuntimeError(f"Checksum mismatch: {destination}")


def configure(source, build, *options):
    run("cmake", "-S", source, "-B", build, "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_POLICY_VERSION_MINIMUM=3.5", "-DCMAKE_CXX_FLAGS=/utf-8 /DNOMINMAX /EHsc", *options)


def build(directory, *targets):
    run("cmake", "--build", directory, "--parallel", os.environ.get("PLUMDECK_BUILD_JOBS", "4"),
        *(["--target", *targets] if targets else []))


def imported_dlls(binary):
    """Return normal and delay-loaded PE imports reported by the MSVC toolchain."""
    output = subprocess.check_output(
        ["dumpbin", "/DEPENDENTS", str(binary)], text=True, errors="replace"
    )
    return sorted(set(re.findall(r"^\s+([^\s]+\.dll)\s*$", output, re.IGNORECASE | re.MULTILINE)))


def stage_runtime_closure(executable, stage, search_roots, plugin_seeds):
    """Copy the executable's recursive DLL closure plus explicitly loaded Qt plugins."""
    candidates = {}
    for root in search_roots:
        if not root or not root.is_dir():
            continue
        for dll in root.rglob("*.dll"):
            candidates.setdefault(dll.name.lower(), dll)

    queue = []
    staged = {}

    def copy(source, destination):
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        staged[source.name.lower()] = destination
        queue.append(destination)

    copy(executable, stage / executable.name)
    for relative, source in plugin_seeds:
        copy(source, stage / relative)

    unresolved = set()
    while queue:
        binary = queue.pop()
        for dependency in imported_dlls(binary):
            key = dependency.lower()
            if key in staged:
                continue
            source = candidates.get(key)
            if source is None:
                unresolved.add(dependency)
                continue
            copy(source, stage / source.name)

    # Windows system DLLs are intentionally unresolved. The relocated smoke test
    # runs with only System32 and this directory available, so any missing
    # redistributable dependency still fails the build.
    if unresolved:
        print("Runtime DLLs supplied by Windows:", ", ".join(sorted(unresolved, key=str.lower)))


def copy_notices(source, destination):
    """Retain redistributable notices without shipping CMake files, docs or locales."""
    if not source.is_dir():
        return
    names = ("license", "copying", "copyright", "notice")
    for notice in source.rglob("*"):
        if not notice.is_file():
            continue
        lower = notice.name.lower()
        if not (lower.startswith(names) or lower.endswith(".spdx.json")):
            continue
        relative = notice.relative_to(source)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(notice, target)


def main():
    if sys.platform != "win32":
        raise SystemExit("Run from an x64 MSVC developer command prompt on Windows")
    DEPS.mkdir(parents=True, exist_ok=True)
    upstream = ROOT / "upstream"
    fetch("https://github.com/mixxxdj/mixxx.git", MIXXX, upstream)
    # Use the hash-verified dependencies from this exact Mixxx revision.
    name = "mixxx-deps-2.5-x64-windows-release-40c29ff"
    archive = DEPS / f"{name}.zip"
    prefix_root = DEPS / name
    if not prefix_root.exists():
        download(f"https://downloads.mixxx.org/dependencies/2.5-rel/Windows/{name}.zip", archive,
                 "a9d809ae9c52d8a553af1bb8a58565649ced7b1f938d1d37c1c7d83ad53aacf3")
        # Python's zipfile still goes through legacy Win32 path handling and
        # fails on Qt's deeply nested object paths in this verified archive.
        # Windows' bundled bsdtar handles those paths and keeps extraction
        # usable from an ordinary developer checkout under the user profile.
        run("tar.exe", "-xf", archive, "-C", DEPS)
    prefix = prefix_root / "installed" / "x64-windows-release"
    if not prefix.is_dir():
        raise RuntimeError("Mixxx dependency archive has an unexpected layout")
    vcpkg = DEPS / "vcpkg-junction"
    fetch("https://github.com/microsoft/vcpkg.git", VCPKG, vcpkg)
    if not (vcpkg / "vcpkg.exe").exists():
        run("cmd", "/c", vcpkg / "bootstrap-vcpkg.bat", "-disableMetrics")
    triplets = DEPS / "triplets"
    triplets.mkdir(exist_ok=True)
    (triplets / "x64-windows-release.cmake").write_text(
        "set(VCPKG_TARGET_ARCHITECTURE x64)\nset(VCPKG_CRT_LINKAGE dynamic)\n"
        "set(VCPKG_LIBRARY_LINKAGE dynamic)\nset(VCPKG_BUILD_TYPE release)\n")
    run(vcpkg / "vcpkg.exe", "install", "libnice:x64-windows-release", "opus:x64-windows-release",
        "--host-triplet=x64-windows-release", f"--overlay-triplets={triplets}", "--clean-after-build")
    extra = vcpkg / "installed" / "x64-windows-release"
    prefixes = f"{prefix.as_posix()};{extra.as_posix()}"
    ldc = DEPS / "libdatachannel"
    fetch("https://github.com/paullouisageneau/libdatachannel.git", LDC, ldc)
    run("git", "-C", ldc, "submodule", "update", "--init", "--recursive", "--depth", "1")
    candidates = list((vcpkg / "downloads" / "tools").rglob("pkgconf.exe"))
    candidates.sort(key=lambda p: ("mingw64" not in p.parts, len(p.parts)))
    pkg = next(iter(candidates), None)
    if pkg is None:
        pkg = shutil.which("pkg-config")
    if not pkg:
        raise RuntimeError("vcpkg's pkgconf executable was not found")
    os.environ["PKG_CONFIG_PATH"] = str(extra / "lib" / "pkgconfig")
    configure(ldc, ldc / "build-windows", f"-DCMAKE_PREFIX_PATH={prefixes}", f"-DPKG_CONFIG_EXECUTABLE={pkg}",
        f"-DCMAKE_INSTALL_PREFIX={(ldc / 'install').as_posix()}", "-DBUILD_SHARED_LIBS=ON", "-DUSE_NICE=ON",
        "-DNO_MEDIA=OFF", "-DNO_WEBSOCKET=OFF", "-DNO_EXAMPLES=ON", "-DNO_TESTS=ON")
    build(ldc / "build-windows")
    run("cmake", "--install", ldc / "build-windows")
    for name, url, filename in [
        ("soundtouch", "https://codeberg.org/soundtouch/soundtouch/archive/2.4.1.tar.gz", "source.tar.gz"),
        ("rubberband", "https://breakfastquay.com/files/releases/rubberband-4.0.0.tar.bz2", "source.tar.bz2"),
    ]:
        download(url, DEPS / name / filename)
    # These preparation scripts check the source archive hashes before extraction.
    run(sys.executable, ROOT / "scripts/prepare-soundtouch.py", DEPS / "soundtouch")
    st = DEPS / "soundtouch"
    configure(st / "source", st / "build", "-DBUILD_SHARED_LIBS=OFF", "-DSOUNDSTRETCH=OFF", "-DSOUNDTOUCH_DLL=OFF", "-DOPENMP=OFF", "-DNEON=OFF")
    build(st / "build")
    rb = DEPS / "rubberband"
    run(sys.executable, ROOT / "scripts/prepare-rubberband.py", rb)
    (rb / "build-config").mkdir(exist_ok=True)
    shutil.copy2(ROOT / "cmake/rubberband-source.cmake", rb / "build-config/CMakeLists.txt")
    configure(rb / "build-config", rb / "build", f"-DCMAKE_PREFIX_PATH={prefixes}",
        f"-DRB_SOURCE={(rb / 'source').as_posix()}")
    build(rb / "build")
    patch = ROOT / "patches/recording-frame-clock.patch"
    applied = subprocess.run(["git", "-C", str(upstream), "apply", "--reverse", "--check", str(patch)], capture_output=True).returncode == 0
    if not applied:
        run("git", "-C", upstream, "apply", "--check", patch)
        run("git", "-C", upstream, "apply", patch)
    all_prefixes = f"{prefixes};{(ldc / 'install').as_posix()}"
    options = [f"-DCMAKE_PREFIX_PATH={all_prefixes}", f"-DMIXXX_VCPKG_ROOT={prefix_root.as_posix()}",
        "-DVCPKG_TARGET_TRIPLET=x64-windows-release", "-DQML=OFF", "-DBUILD_TESTING=OFF", "-DBUILD_BENCH=OFF",
        "-DENGINEPRIME=OFF", "-DKEYFINDER=OFF", "-DPORTMIDI=OFF", "-DBROADCAST=OFF", "-DQTKEYCHAIN=OFF",
        "-DHID=OFF", "-DBULK=OFF", "-DVINYLCONTROL=OFF", "-DFFMPEG=OFF", "-DMAD=ON", "-DBATTERY=OFF", "-DLILV=OFF",
        "-DOPTIMIZE=portable", "-DWARNINGS_FATAL=OFF",
        f"-DCMAKE_PROJECT_mixxx_INCLUDE={(ROOT / 'cmake/inject-host.cmake').as_posix()}"]
    configure(upstream, ROOT / "build-upstream", *options)
    build(ROOT / "build-upstream", "plumdeck-mixxx-engine-host")
    configure(ROOT, ROOT / "build-seam", f"-DCMAKE_PREFIX_PATH={all_prefixes}")
    build(ROOT / "build-seam")
    # Stage a complete relocatable directory. The dependency archives contain
    # every Mixxx/Qt development DLL and more than 100 MiB of GUI resources;
    # this headless host must ship only its recursive PE closure and the Qt
    # plugins it loads by name.
    stage = ROOT / "stage" / "PlumdeckMixxxHost"
    shutil.rmtree(stage, ignore_errors=True)
    stage.mkdir(parents=True, exist_ok=True)
    executable = ROOT / "build-upstream/plumdeck-mixxx-engine-host.exe"
    plugins = [
        (Path("platforms/qoffscreen.dll"), next(prefix.rglob("qoffscreen.dll"))),
        (Path("sqldrivers/qsqlite.dll"), next(prefix.rglob("qsqlite.dll"))),
        (Path("tls/qschannelbackend.dll"), next(prefix.rglob("qschannelbackend.dll"))),
    ]
    search_roots = [prefix / "bin", extra / "bin", ldc / "install/bin"]
    # windeployqt previously copied vc_redist.x64.exe into the application but
    # nothing executed it. Copy the imported VC runtime DLLs app-locally instead.
    redist = os.environ.get("VCToolsRedistDir")
    if redist:
        search_roots.append(Path(redist) / "x64")
    stage_runtime_closure(executable, stage, search_roots, plugins)

    # The host creates no window, skin, library or controller subsystem. Effects
    # and the settings schema are the only upstream runtime resources it uses.
    shutil.copytree(upstream / "res/effects", stage / "res/effects")
    shutil.copy2(upstream / "res/schema.xml", stage / "res/schema.xml")

    copy_notices(extra / "share", stage / "licenses/junction")
    copy_notices(prefix / "share", stage / "licenses/mixxx-dependencies")
    shutil.copy2(upstream / "LICENSE", stage / "LICENSE-Mixxx")
    # These libraries are built outside the Mixxx/vcpkg prefixes. Retain their
    # notices as well, including libdatachannel's bundled dependencies.
    for name, source in (("SoundTouch", st / "source"), ("RubberBand", rb / "source"),
                         ("libdatachannel", ldc)):
        copy_notices(source, stage / "licenses" / name)
    shutil.copy2(ROOT / "dependency-versions.json", stage / "dependency-versions.json")
    staged_files = [path for path in stage.rglob("*") if path.is_file()]
    staged_bytes = sum(path.stat().st_size for path in staged_files)
    print(f"Staged Windows native engine: {len(staged_files)} files, {staged_bytes / 1024 / 1024:.1f} MiB")
    if len(staged_files) > 500 or staged_bytes > 160 * 1024 * 1024:
        raise RuntimeError("Windows native stage unexpectedly contains development or GUI payloads")
    os.environ["PATH"] = str(stage) + os.pathsep + os.environ["PATH"]
    run("ctest", "--test-dir", ROOT / "build-seam", "--output-on-failure")
    run("node", ROOT / "scripts/smoke-bundle.mjs", stage / "plumdeck-mixxx-engine-host.exe")


if __name__ == "__main__":
    main()
