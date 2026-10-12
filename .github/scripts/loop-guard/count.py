#!/usr/bin/env python3
"""count.py BIN COUNTS TRACE

Reads what `step` wrote of a silt binary that was running a loop, and
prints two numbers per round of the loop:

- the machine instructions executed, and
- the loads that cannot be forwarded: a load that overlaps a store made
  within the last 60 executed instructions without that one store
  covering all of it (a 16-byte load of bytes that a 1-byte and an
  8-byte store wrote). The processor cannot serve such a load from its
  store buffer; it waits until the stores have reached the cache, 12 to
  14 cycles on the machine this was measured on.

The last line is `instructions <n> loads <m>`, with one decimal.
"""
import bisect
import collections
import re
import struct
import subprocess
import sys

WINDOW = 60
REGISTERS = ['r15', 'r14', 'r13', 'r12', 'rbp', 'rbx', 'r11', 'r10', 'r9', 'r8', 'rax', 'rcx',
             'rdx', 'rsi', 'rdi', 'orig_rax', 'rip', 'cs', 'eflags', 'rsp', 'ss', 'fs_base',
             'gs_base', 'ds', 'es', 'fs', 'gs']
COMPARES = ('cmp', 'test', 'ucomis', 'comis', 'bt')
READS_AND_WRITES = ('add', 'sub', 'and', 'or', 'xor', 'inc', 'dec', 'neg', 'not', 'shl', 'shr',
                    'sar', 'adc', 'sbb', 'xadd', 'xchg', 'cmpxchg', 'lock')


def main():
    binary, counts_path, trace_path = sys.argv[1:4]
    lines = open(counts_path).read().split('\n')
    base = int(lines[0].split()[1], 16)
    counts = {int(a, 16): int(c) for a, c in (line.split() for line in lines[1:] if line)}
    symbols = functions(binary)

    # The rounds in the window: the least count that many instructions
    # share. (An instruction on the round's own path runs once a round;
    # one of the dispatch runs once per bytecode instruction.)
    shared = collections.Counter(c for c in counts.values() if c > 50)
    rounds = min(c for c, instructions in shared.items() if instructions >= 12)
    per_round = sum(counts.values()) / rounds

    # The window must lie in the loop: nearly all of it in the function
    # that runs silt code and what it calls every round.
    by_function = collections.Counter()
    for address, count in counts.items():
        by_function[symbols.name(address)] += count
    in_loop = sum(c for name, c in by_function.items() if c >= rounds / 2)
    if in_loop < 0.98 * sum(counts.values()):
        sys.exit('count.py: the stepped window is not inside the loop')
    for name, count in by_function.most_common(6):
        print('  %8.2f per round  %s' % (count / rounds, name[:100]))

    loads = unforwardable(binary, symbols, base, trace_path)
    traced_rounds = loads.steps / per_round
    for address, count in loads.sites.most_common(8):
        text, size, stores = loads.what[address]
        print('  %5.1f per round  %x: %s (%d bytes) after %s'
              % (count / traced_rounds, address, text, size,
                 ' + '.join('%s [%d]' % store for store in stores)[:140]))
    print('instructions %.1f loads %.1f' % (per_round, sum(loads.sites.values()) / traced_rounds))


class functions:
    """The functions of the binary, by address."""

    def __init__(self, binary):
        out = subprocess.run(['nm', '-C', '-S', '--defined-only', binary],
                             capture_output=True, text=True, check=True).stdout
        self.rows = []
        for line in out.splitlines():
            parts = line.split(' ', 3)
            if len(parts) == 4 and parts[2] in 'tTwW':
                self.rows.append((int(parts[0], 16), int(parts[1], 16), parts[3]))
        self.rows.sort()
        self.starts = [row[0] for row in self.rows]

    def row(self, address):
        i = bisect.bisect_right(self.starts, address) - 1
        if i >= 0 and address < self.rows[i][0] + max(self.rows[i][1], 1):
            return self.rows[i]
        return (address, 1, '?')

    def name(self, address):
        return self.row(address)[2]


def register_size(register):
    register = register.lstrip('%')
    if register.startswith('xmm'):
        return 16
    if re.fullmatch(r'r(\d+|[a-ds][xpi]|[sd]i|[sb]p)', register):
        return 8
    if re.fullmatch(r'e[a-ds][xpi]|e[sd]i|e[sb]p|r\d+d', register):
        return 4
    if re.fullmatch(r'[a-ds][xpi]|[sd]i|[sb]p|r\d+w', register):
        return 2
    return 1


def accesses(text, registers):
    """The memory an instruction reads ('L') and writes ('S'): (kind, address, size)."""
    op, _, arguments = text.partition(' ')
    found = []
    if op in ('push', 'call'):
        found.append(('S', registers['rsp'] - 8, 8))
    if op in ('pop', 'ret'):
        found.append(('L', registers['rsp'], 8))
    if op == 'lea' or op.startswith(('nop', 'prefetch')) or 'rip' in arguments or '%fs:' in arguments:
        return found
    parts = re.split(r',(?![^(]*\))', arguments) if arguments else []
    for k, part in enumerate(parts):
        m = re.fullmatch(r'\*?(-?0x[0-9a-f]+|-?\d+)?\((%\w+)?(?:,(%\w+)(?:,(\d))?)?\)', part.strip())
        if not m:
            continue
        base = registers.get(m.group(2).lstrip('%')) if m.group(2) else 0
        index = registers.get(m.group(3).lstrip('%')) if m.group(3) else 0
        if base is None or index is None:
            return found
        address = (int(m.group(1) or '0', 0) + base + index * int(m.group(4) or 1)) % 2**64
        others = [p.strip() for j, p in enumerate(parts) if j != k and p.strip().startswith('%')]
        if op in ('movzbl', 'movzbq', 'movsbl', 'movsbq', 'movzbw'):
            size = 1
        elif op in ('movzwl', 'movzwq', 'movswl', 'movswq'):
            size = 2
        elif op in ('movslq', 'movss', 'movd', 'cvtss2sd'):
            size = 4
        elif op in ('movsd', 'movq', 'movlpd', 'movhpd', 'cvtsd2ss', 'addsd', 'subsd', 'mulsd',
                    'divsd', 'ucomisd', 'comisd', 'cvttsd2si', 'cvtsi2sd', 'cvtsi2sdq'):
            size = 8
        elif others:
            size = register_size(others[-1])
        elif op[-1] in 'bwlq' and op[:-1] in ('mov', 'cmp', 'test', 'add', 'sub', 'and', 'or', 'xor',
                                              'inc', 'dec', 'neg', 'not', 'shl', 'shr', 'sar',
                                              'adc', 'sbb', 'imul', 'set'):
            size = {'b': 1, 'w': 2, 'l': 4, 'q': 8}[op[-1]]
        elif op.startswith('set'):
            size = 1
        else:
            size = 8
        if op.startswith(('j', 'call')):
            found.append(('L', address, 8))
        elif k == len(parts) - 1 and len(parts) > 1 and not op.startswith(COMPARES):
            if op.startswith(READS_AND_WRITES):
                found.append(('L', address, size))
            found.append(('S', address, size))
        elif len(parts) == 1 and op.startswith(('inc', 'dec', 'neg', 'not', 'set')):
            if not op.startswith('set'):
                found.append(('L', address, size))
            found.append(('S', address, size))
        else:
            found.append(('L', address, size))
    return found


class unforwardable:
    """The loads of the trace that overlap a recent store which does not cover them."""

    def __init__(self, binary, symbols, base, trace_path):
        data = open(trace_path, 'rb').read()
        self.steps = len(data) // 216
        self.sites = collections.Counter()
        self.what = {}
        text = {}
        stores = collections.deque()
        for step in range(self.steps):
            registers = dict(zip(REGISTERS, struct.unpack_from('<27Q', data, step * 216)))
            address = registers['rip'] - base
            if address not in text:
                disassemble(binary, symbols.row(address), text)
                text.setdefault(address, '?')
            while stores and stores[0][0] < step - WINDOW:
                stores.popleft()
            for kind, at, size in accesses(text[address], registers):
                if kind == 'S':
                    stores.append((step, at, size, address))
                    continue
                over = [s for s in stores if s[1] < at + size and at < s[1] + s[2]]
                if not over:
                    continue
                last = over[-1]
                if last[1] <= at and at + size <= last[1] + last[2]:
                    continue
                self.sites[address] += 1
                self.what[address] = (text[address], size, sorted({(text[s[3]], s[2]) for s in over}))


def disassemble(binary, row, text):
    start, size, _ = row
    out = subprocess.run(['objdump', '-d', '--no-show-raw-insn', '--start-address=%#x' % start,
                          '--stop-address=%#x' % (start + max(size, 1)), binary],
                         capture_output=True, text=True, check=True).stdout
    for line in out.splitlines():
        m = re.match(r'\s*([0-9a-f]+):\t(.*)', line)
        if m:
            text[int(m.group(1), 16)] = re.sub(r'\s+', ' ', re.sub(r'\s*[<#].*$', '', m.group(2))).strip()


if __name__ == '__main__':
    main()
