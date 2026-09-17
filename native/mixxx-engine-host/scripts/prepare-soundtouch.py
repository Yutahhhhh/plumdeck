"""Extract the pinned SoundTouch source after checking the archive hash."""
import hashlib
from pathlib import Path
import sys
import tarfile

root = Path(sys.argv[1])
archive = root / 'source.tar.gz'
assert hashlib.sha256(archive.read_bytes()).hexdigest() == '35d404e6e8c2ebd12fb4000da6fadd75c99e37eed2126a04721828c11c0377ec'
with tarfile.open(archive) as source:
    for item in source.getmembers():
        relative = Path(item.name).relative_to('soundtouch')
        if not item.isfile():
            continue
        assert '..' not in relative.parts
        data = source.extractfile(item).read()
        target = root / 'source' / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.exists() or target.read_bytes() != data:
            target.write_bytes(data)
        if relative.parts[0] == 'include':
            public = root / 'include' / 'soundtouch' / relative.name
            public.parent.mkdir(parents=True, exist_ok=True)
            if not public.exists() or public.read_bytes() != data:
                public.write_bytes(data)
