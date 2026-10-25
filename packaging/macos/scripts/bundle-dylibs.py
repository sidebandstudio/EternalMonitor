#!/usr/bin/env python3
"""Copy the executable's non-system dylibs into the app and repoint them.

    bundle-dylibs.py APP EXECUTABLE PREFIX LICENCE_LIST

Every library the executable needs from PREFIX (Homebrew's /opt/homebrew),
directly or through another library (or, for LOADED_AT_RUN_TIME, by name at
run time), is copied into APP/Contents/Frameworks
under the name it is linked by. Each copy gets the install name
@rpath/<name> and an @loader_path run path, every reference to it becomes
@rpath/<name>, and the executable gets @executable_path/../Frameworks. The
copies are no longer validly signed afterwards; build-app.sh signs them.

LICENCE_LIST gets one line per bundled library: its name, the Homebrew
formula and version it came from, and the licence Homebrew records for that
formula.
"""
import json
import os
import shutil
import subprocess
import sys

SYSTEM = ("/usr/lib/", "/System/")
# Libraries a bundled library loads by itself at run time, with no load
# command to follow: the name it looks for beside itself, and the file under
# PREFIX. sdl2-compat (libavdevice's SDL output) loads SDL3 from its
# initializer, and without it shows an error dialog before the host starts.
LOADED_AT_RUN_TIME = {
    "libSDL2-2.0.0.dylib": {"libSDL3.dylib": "opt/sdl3/lib/libSDL3.dylib"},
}


def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def load_commands(path):
    """(dependencies, run paths) from `otool -l`."""
    deps, rpaths, kind = [], [], None
    for line in run("otool", "-l", path).splitlines():
        line = line.strip()
        if line.startswith("cmd "):
            kind = line.split()[1]
        elif line.startswith("name ") and kind in ("LC_LOAD_DYLIB", "LC_LOAD_WEAK_DYLIB", "LC_REEXPORT_DYLIB"):
            deps.append(line.split()[1])
        elif line.startswith("path ") and kind == "LC_RPATH":
            rpaths.append(line.split()[1])
    return deps, rpaths


def find(reference, loader, loader_rpaths, prefix):
    """The file a reference resolves to, if it is one of PREFIX's."""
    if reference.startswith(SYSTEM):
        return None
    if reference.startswith("@rpath/"):
        name = reference[len("@rpath/"):]
        for rpath in loader_rpaths:
            base = rpath.replace("@loader_path", os.path.dirname(loader))
            candidate = os.path.join(base, name)
            if os.path.exists(candidate):
                return os.path.realpath(candidate)
        # Homebrew libraries refer to siblings in their own lib directory.
        candidate = os.path.join(os.path.dirname(loader), name)
        if os.path.exists(candidate):
            return os.path.realpath(candidate)
        raise SystemExit(f"{loader}: cannot resolve {reference}")
    if reference.startswith("@"):
        raise SystemExit(f"{loader}: unexpected reference {reference}")
    if not reference.startswith(prefix):
        raise SystemExit(f"{loader}: links {reference}, outside {prefix} and the system")
    return os.path.realpath(reference)


def formula_of(path):
    """(formula, version) from a Cellar path."""
    parts = path.split("/")
    cellar = parts.index("Cellar")
    return parts[cellar + 1], parts[cellar + 2]


def main():
    app, executable, prefix, licence_list = sys.argv[1:]
    frameworks = os.path.join(app, "Contents", "Frameworks")
    os.makedirs(frameworks, exist_ok=True)

    # Breadth first from the executable: name -> real file.
    bundled = {}
    queue = [executable]
    while queue:
        loader = queue.pop(0)
        real_loader = loader if loader == executable else bundled[loader]
        deps, rpaths = load_commands(real_loader)
        for reference in deps:
            found = find(reference, real_loader, rpaths, prefix)
            if found is None:
                continue
            name = os.path.basename(reference)
            if name in bundled:
                if bundled[name] != found:
                    raise SystemExit(f"two libraries named {name}: {bundled[name]} and {found}")
                continue
            bundled[name] = found
            queue.append(name)
            for extra, source in LOADED_AT_RUN_TIME.get(name, {}).items():
                path = os.path.realpath(os.path.join(prefix, source))
                if not os.path.exists(path):
                    raise SystemExit(f"{name} needs {extra} at run time; {path} is missing")
                bundled[extra] = path
                queue.append(extra)

    def repoint(path, is_executable):
        deps, rpaths = load_commands(path)
        args = []
        for reference in deps:
            name = os.path.basename(reference)
            if name in bundled and reference != f"@rpath/{name}":
                args += ["-change", reference, f"@rpath/{name}"]
        wanted = "@executable_path/../Frameworks" if is_executable else "@loader_path"
        for rpath in rpaths:
            if rpath != wanted:
                args += ["-delete_rpath", rpath]
        if wanted not in rpaths:
            args += ["-add_rpath", wanted]
        if not is_executable:
            args += ["-id", f"@rpath/{os.path.basename(path)}"]
        # This invalidates the copy's existing signature (install_name_tool
        # warns); build-app.sh signs every file again. Removing the old
        # signature first instead leaves __LINKEDIT in a shape
        # install_name_tool refuses.
        result = subprocess.run(["install_name_tool", *args, path], capture_output=True, text=True)
        if result.returncode != 0:
            raise SystemExit(f"install_name_tool failed on {path}:\n{result.stderr}")

    for name, source in sorted(bundled.items()):
        target = os.path.join(frameworks, name)
        shutil.copyfile(source, target)
        os.chmod(target, 0o755)
        repoint(target, False)
    repoint(executable, True)

    formulae = sorted({formula_of(source)[0] for source in bundled.values()})
    info = json.loads(run("brew", "info", "--json=v2", "--formula", *formulae))
    licences = {f["name"]: f.get("license") or "unknown" for f in info["formulae"]}
    with open(licence_list, "w") as out:
        out.write("Libraries bundled in Contents/Frameworks, from Homebrew\n")
        out.write("library\tformula\tversion\tlicence (Homebrew's record)\n")
        for name, source in sorted(bundled.items()):
            formula, version = formula_of(source)
            out.write(f"{name}\t{formula}\t{version}\t{licences.get(formula, 'unknown')}\n")
    print(f"bundled {len(bundled)} libraries from {len(formulae)} formulae")


if __name__ == "__main__":
    main()
