"""Copy only build inputs into the Sites source checkout; never runtime data."""
from pathlib import Path
import hashlib,json,shutil

web=Path(__file__).resolve().parents[1]
workspace=web.parent
destination=web/'source'
inputs=['Cargo.toml','Cargo.lock','README.md','.env.example',
        'server/Cargo.toml','server/src','shared/Cargo.toml','shared/src',
        'dioxus-client/Cargo.toml','dioxus-client/Cargo.lock','dioxus-client/build.rs',
        'dioxus-client/Dioxus.toml','dioxus-client/AGENTS.md','dioxus-client/src',
        'dioxus-client/assets','dioxus-client/vendor','dioxus-client/ios','dioxus-client/tests']
if destination.exists():shutil.rmtree(destination)
for relative in inputs:
    src=workspace/relative;target=destination/relative
    target.parent.mkdir(parents=True,exist_ok=True)
    if src.is_dir():shutil.copytree(src,target,ignore=shutil.ignore_patterns('.DS_Store','.git','target','.signing','*.log'))
    else:shutil.copy2(src,target)
files={str(p.relative_to(destination)):hashlib.sha256(p.read_bytes()).hexdigest()
       for p in sorted(destination.rglob('*')) if p.is_file()}
(destination/'build-inputs.json').write_text(json.dumps(files,indent=2)+'\n')
print('Prepared sanitized Rust/Dioxus source snapshot ('+str(len(files))+' files).')
