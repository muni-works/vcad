"""OCCT reference metrics for one STEP file (pip install cadquery-ocp).

    python occt_ref.py file.step  ->  one JSON line

Reads with the XDE reader (STEPCAFControl) so colours, names and assembly
structure count, exactly as FreeCAD would see them.
"""
import json
import sys
import time

from OCP.BRepCheck import BRepCheck_Analyzer
from OCP.BRepGProp import BRepGProp
from OCP.GProp import GProp_GProps
from OCP.IFSelect import IFSelect_RetDone
from OCP.STEPCAFControl import STEPCAFControl_Reader
from OCP.TCollection import TCollection_ExtendedString
try:
    from OCP.TDF import TDF_LabelSequence
except ImportError:  # OCP >= 7.8 moved NCollection sequences
    from OCP.OCP.collections import Sequence_TDF_Label as TDF_LabelSequence
from OCP.TDocStd import TDocStd_Document
from OCP.TopAbs import TopAbs_FACE, TopAbs_SOLID
from OCP.TopExp import TopExp_Explorer
from OCP.XCAFDoc import XCAFDoc_DocumentTool, XCAFDoc_ColorType, XCAFDoc_ColorTool
from OCP.Quantity import Quantity_Color


def count(shape, kind):
    n, e = 0, TopExp_Explorer(shape, kind)
    while e.More():
        n += 1
        e.Next()
    return n


def main(path):
    t = time.time()
    doc = TDocStd_Document(TCollection_ExtendedString("XmlOcaf"))
    r = STEPCAFControl_Reader()
    r.SetColorMode(True)
    r.SetNameMode(True)
    if r.ReadFile(path) != IFSelect_RetDone or not r.Transfer(doc):
        print(json.dumps({"occt_ok": False}))
        return
    st = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())
    ct = XCAFDoc_DocumentTool.ColorTool_s(doc.Main())
    free = TDF_LabelSequence()
    st.GetFreeShapes(free)
    shapes = TDF_LabelSequence()
    st.GetShapes(shapes)
    faces = solids = 0
    vol = 0.0
    valid = True
    asm = comps = 0
    colored = 0
    for i in range(1, free.Length() + 1):
        s = st.GetShape_s(free.Value(i))
        faces += count(s, TopAbs_FACE)
        solids += count(s, TopAbs_SOLID)
        p = GProp_GProps()
        BRepGProp.VolumeProperties_s(s, p)
        vol += p.Mass()
        valid = valid and BRepCheck_Analyzer(s).IsValid()
    c = Quantity_Color()
    for i in range(1, shapes.Length() + 1):
        lab = shapes.Value(i)
        if st.IsAssembly_s(lab):
            asm += 1
            kids = TDF_LabelSequence()
            st.GetComponents_s(lab, kids, False)
            comps += kids.Length()
        for ty in (XCAFDoc_ColorType.XCAFDoc_ColorSurf, XCAFDoc_ColorType.XCAFDoc_ColorGen):
            if XCAFDoc_ColorTool.GetColor_s(lab, ty, c):
                colored += 1
                break
    print(json.dumps({
        "occt_ok": True, "occt_ms": round((time.time() - t) * 1e3),
        "occt_faces": faces, "occt_solids": solids, "occt_volume": round(vol, 3),
        "occt_valid": valid, "occt_assemblies": asm, "occt_components": comps,
        "occt_colored_shapes": colored,
    }))


if __name__ == "__main__":
    main(sys.argv[1])
