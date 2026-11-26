#!/usr/bin/env python3
"""Exercise the exact production receipt state without a compositor or GUI."""
from pathlib import Path
import argparse,subprocess,tempfile
root=Path(__file__).resolve().parents[1]
source=root/'vendor/winit/src/platform_impl/linux/wayland/types/wp_presentation.rs'
text=source.read_text();state=text.split('// BEGIN native receipt state\n',1)[1].split('// END native receipt state',1)[0]
tests=text[text.index('#[cfg(test)]'):]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--rustc',default='rustc');args=parser.parse_args()
with tempfile.TemporaryDirectory(prefix='wayland-receipt-state-') as directory:
    root=Path(directory);unit=root/'receipt.rs';unit.write_text(state+tests)
    binary=root/'receipt-tests'
    subprocess.run([args.rustc,'--edition=2021','--test',str(unit),'-o',str(binary)],check=True)
    subprocess.run([str(binary),'--nocapture'],check=True)

# source is .../winit/src/platform_impl/linux/wayland/types/wp_presentation.rs;
# use the project root directly instead of relying on vendor nesting details.
publication=Path(__file__).resolve().parents[1]/'vendor/wgpu-hal/src/vulkan/publication.rs'
with tempfile.TemporaryDirectory(prefix='vulkan-publication-state-') as directory:
    unit=Path(directory)/'publication.rs';unit.write_text('mod publication {\n'+publication.read_text()+'\n}')
    binary=Path(directory)/'publication-tests'
    subprocess.run([args.rustc,'--edition=2021','--test',str(unit),'-o',str(binary)],check=True)
    subprocess.run([str(binary),'--nocapture'],check=True)
