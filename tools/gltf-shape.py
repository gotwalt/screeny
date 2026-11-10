#!/usr/bin/env python3
"""Keep only a skinned, animated glTF's shape: positions, joints, weights,
indices, the skin and its animations. Normals, texture coordinates,
materials, textures, cameras and lights go. Writes a .glb.

    python3 -I tools/gltf-shape.py IN.gltf OUT.glb

Made for dove (crates/art/src/patches/dove), which lights a model only by
how near the glass it is, so a model's colours and normals are dead weight.
Reads one .gltf whose buffers are data: URIs or files beside it.
"""
import base64
import json
import os
import struct
import sys

SIZES = {5120: 1, 5121: 1, 5122: 2, 5123: 2, 5125: 4, 5126: 4}
WIDTH = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}


def main(src, dst):
    g = json.load(open(src))
    base = os.path.dirname(src)
    buffers = []
    for b in g["buffers"]:
        uri = b["uri"]
        if uri.startswith("data:"):
            buffers.append(base64.b64decode(uri.split(",", 1)[1]))
        else:
            buffers.append(open(os.path.join(base, uri), "rb").read())

    def raw(i):
        a = g["accessors"][i]
        v = g["bufferViews"][a["bufferView"]]
        size = SIZES[a["componentType"]] * WIDTH[a["type"]]
        stride = v.get("byteStride", size)
        start = v.get("byteOffset", 0) + a.get("byteOffset", 0)
        data = buffers[v["buffer"]]
        return b"".join(data[start + k * stride : start + k * stride + size] for k in range(a["count"]))

    keep = {}  # old accessor -> new index
    accessors, views, blob = [], [], bytearray()

    def take(i):
        if i in keep:
            return keep[i]
        data = raw(i)
        while len(blob) % 4:
            blob.append(0)
        views.append({"buffer": 0, "byteOffset": len(blob), "byteLength": len(data)})
        blob.extend(data)
        a = {k: v for k, v in g["accessors"][i].items() if k not in ("bufferView", "byteOffset", "sparse")}
        a["bufferView"] = len(views) - 1
        accessors.append(a)
        keep[i] = len(accessors) - 1
        return keep[i]

    meshes = []
    for m in g["meshes"]:
        prims = []
        for p in m["primitives"]:
            attrs = {k: take(v) for k, v in p["attributes"].items() if k in ("POSITION", "JOINTS_0", "WEIGHTS_0")}
            prims.append({"attributes": attrs, "indices": take(p["indices"])})
        meshes.append({"name": m.get("name", ""), "primitives": prims})
    skins = [dict(s, inverseBindMatrices=take(s["inverseBindMatrices"])) for s in g.get("skins", [])]
    nodes = [{k: v for k, v in n.items() if k not in ("camera", "extensions")} for n in g["nodes"]]
    animations = []
    for a in g.get("animations", []):
        # A channel that never changes is no animation: its one value
        # becomes the node's own.
        channels, samplers = [], []
        for c in a["channels"]:
            s = a["samplers"][c["sampler"]]
            out = g["accessors"][s["output"]]
            values = struct.unpack(f"<{out['count'] * WIDTH[out['type']]}f", raw(s["output"]))
            w = WIDTH[out["type"]]
            first = values[:w]
            if all(abs(v - first[k % w]) < 1e-6 for k, v in enumerate(values)) and "matrix" not in nodes[c["target"]["node"]]:
                nodes[c["target"]["node"]][c["target"]["path"]] = list(first)
                continue
            samplers.append(dict(s, input=take(s["input"]), output=take(s["output"])))
            channels.append({"sampler": len(samplers) - 1, "target": c["target"]})
        animations.append({"name": a.get("name", ""), "channels": channels, "samplers": samplers})

    out = {
        "asset": {"version": "2.0", "generator": "screeny tools/gltf-shape.py"},
        "scene": 0,
        "scenes": g["scenes"],
        "nodes": nodes,
        "meshes": meshes,
        "skins": skins,
        "animations": animations,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": len(blob)}],
    }
    js = json.dumps(out, separators=(",", ":")).encode()
    js += b" " * (-len(js) % 4)
    blob += b"\0" * (-len(blob) % 4)
    total = 12 + 8 + len(js) + 8 + len(blob)
    with open(dst, "wb") as f:
        f.write(struct.pack("<III", 0x46546C67, 2, total))
        f.write(struct.pack("<II", len(js), 0x4E4F534A) + js)
        f.write(struct.pack("<II", len(blob), 0x004E4942) + bytes(blob))
    print(f"{dst}: {total} bytes, {len(accessors)} accessors")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
