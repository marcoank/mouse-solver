"""Estrae i profili dei mouse da rivalcfg (licenza WTFPL) come JSON, SENZA eseguire il suo codice.

rivalcfg descrive ogni mouse con un dizionario in rivalcfg/devices/*.py. Qui quei file vengono letti
con l'AST e valutati da un mini-interprete che capisce solo dati: letterali, nomi, attributi di altri
moduli di rivalcfg, indici, operatori su interi e le dict comprehension usate per le versioni wireless.
Tutto il resto (lambda, chiamate sconosciute) diventa None.

Uso:  python -I tools/rivalcfg_dump.py <cartella rivalcfg>  > profili.json
"""
import ast
import json
import operator
import sys
from pathlib import Path

USBHID = {"HID_REPORT_TYPE_OUTPUT": "output", "HID_REPORT_TYPE_FEATURE": "feature"}
BINOPS = {ast.BitOr: operator.or_, ast.BitAnd: operator.and_, ast.Add: operator.add, ast.Sub: operator.sub,
          ast.Mult: operator.mul, ast.LShift: operator.lshift, ast.RShift: operator.rshift}


class Unsupported(Exception):
    pass


class Module:
    def __init__(self, root, dotted, cache):
        self.root, self.dotted, self.cache = root, dotted, cache
        self.env = {"usbhid": USBHID}
        self.funcs = {}
        path = root / (dotted.replace(".", "/") + ".py")
        tree = ast.parse(path.read_text(encoding="utf-8"))
        for st in tree.body:
            if isinstance(st, ast.ImportFrom):
                base = dotted.rsplit(".", 1)[0]
                pkg = base + ("." + st.module if st.module else "") if st.level == 1 else None
                for alias in st.names:
                    if pkg is None or alias.name == "usbhid":
                        continue
                    target = f"{pkg}.{alias.name}"
                    if (root / (target.replace(".", "/") + ".py")).exists():
                        self.env[alias.asname or alias.name] = load(root, target, cache).env
            elif isinstance(st, ast.FunctionDef):
                self.funcs[st.name] = st
            elif isinstance(st, ast.Assign) and len(st.targets) == 1 and isinstance(st.targets[0], ast.Name):
                try:
                    self.env[st.targets[0].id] = self.eval(st.value, {})
                except Unsupported:
                    pass

    def call_patch(self, fn, arg):
        """Emula le funzioni _patch_command delle versioni wireless (copia + OR sul primo byte + readback)."""
        src = ast.unparse(fn)
        if "_WIRELESS_FLAG" not in src:
            raise Unsupported(fn.name)
        result = dict(arg)
        result["command"] = list(result["command"])
        result["command"][0] |= self.env["_WIRELESS_FLAG"]
        if "readback_length" in src:
            result["readback_length"] = self.env.get("_READBACK_LENGTH", 64)
        return result

    def eval(self, n, local):
        if isinstance(n, ast.Constant):
            return n.value
        if isinstance(n, ast.Dict):
            out = {}
            for k, v in zip(n.keys, n.values):
                try:
                    out[self.eval(k, local)] = self.eval(v, local)
                except Unsupported:
                    out[self.eval(k, local)] = None
            return out
        if isinstance(n, (ast.List, ast.Tuple)):
            out = []
            for e in n.elts:
                if isinstance(e, ast.Starred):
                    out.extend(self.eval(e.value, local))
                else:
                    out.append(self.eval(e, local))
            return out
        if isinstance(n, ast.Name):
            if n.id in local:
                return local[n.id]
            if n.id in self.env:
                return self.env[n.id]
            raise Unsupported(n.id)
        if isinstance(n, ast.Attribute):
            base = self.eval(n.value, local)
            if isinstance(base, dict) and n.attr in base:
                return base[n.attr]
            raise Unsupported(n.attr)
        if isinstance(n, ast.Subscript):
            return self.eval(n.value, local)[self.eval(n.slice, local)]
        if isinstance(n, ast.BinOp) and type(n.op) in BINOPS:
            return BINOPS[type(n.op)](self.eval(n.left, local), self.eval(n.right, local))
        if isinstance(n, ast.UnaryOp) and isinstance(n.op, ast.USub):
            return -self.eval(n.operand, local)
        if isinstance(n, ast.DictComp) and len(n.generators) == 1:
            g = n.generators[0]
            it = g.iter
            if not (isinstance(it, ast.Call) and isinstance(it.func, ast.Attribute) and it.func.attr == "items"):
                raise Unsupported("dictcomp")
            src = self.eval(it.func.value, local)
            names = [e.id for e in g.target.elts]
            out = {}
            for k, v in src.items():
                loc = dict(local, **{names[0]: k, names[1]: v})
                out[self.eval(n.key, loc)] = self.eval(n.value, loc)
            return out
        if isinstance(n, ast.Call) and isinstance(n.func, ast.Name) and n.func.id in self.funcs and len(n.args) == 1:
            return self.call_patch(self.funcs[n.func.id], self.eval(n.args[0], local))
        raise Unsupported(type(n).__name__)


def load(root, dotted, cache):
    if dotted not in cache:
        cache[dotted] = Module(root, dotted, cache)
    return cache[dotted]


def main():
    root = Path(sys.argv[1])
    cache = {}
    out = {}
    for f in sorted((root / "rivalcfg" / "devices").glob("*.py")):
        if f.name.startswith("_"):
            continue
        m = load(root, f"rivalcfg.devices.{f.stem}", cache)
        if "profile" in m.env:
            out[f.stem] = m.env["profile"]
        else:
            print(f"impossibile leggere {f.name}", file=sys.stderr)
    json.dump(out, sys.stdout, indent=1)


if __name__ == "__main__":
    main()
