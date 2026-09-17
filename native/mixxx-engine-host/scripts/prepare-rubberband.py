"""Extract the pinned RubberBand source after checking the archive hash."""
import hashlib
from pathlib import Path
import sys
import tarfile
root=Path(sys.argv[1])
archive=root/'source.tar.bz2'
assert hashlib.sha256(archive.read_bytes()).hexdigest()=='af050313ee63bc18b35b2e064e5dce05b276aaf6d1aa2b8a82ced1fe2f8028e9'
with tarfile.open(archive) as source:
 for item in source.getmembers():
  relative=Path(item.name).relative_to('rubberband-4.0.0')
  if not item.isfile():continue
  assert '..' not in relative.parts
  data=source.extractfile(item).read()
  target=root/'source'/relative;target.parent.mkdir(parents=True,exist_ok=True)
  if not target.exists() or target.read_bytes()!=data:target.write_bytes(data)
