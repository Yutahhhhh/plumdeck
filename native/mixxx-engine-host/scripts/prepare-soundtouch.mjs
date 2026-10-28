// Extract the pinned SoundTouch source after checking the archive hash, and
// mirror its public headers into include/soundtouch/.
// Usage: node prepare-soundtouch.mjs <dependency root containing source.tar.gz>
import { join, resolve } from 'node:path';
import { extractPinnedSource, writeIfChanged } from './extract-pinned-source.mjs';

if (!process.argv[2]) throw new Error('Usage: node prepare-soundtouch.mjs <dependency root>');
const root = resolve(process.argv[2]);
extractPinnedSource({
    root,
    archive: 'source.tar.gz',
    prefix: 'soundtouch',
    sha256: '35d404e6e8c2ebd12fb4000da6fadd75c99e37eed2126a04721828c11c0377ec',
    onFile(parts, data) {
        if (parts[0] === 'include') writeIfChanged(join(root, 'include', 'soundtouch', parts.at(-1)), data);
    },
});
