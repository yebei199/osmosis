"""核对本套件的验收草案与实际 JUnit，拒绝空集合、重复、跳过或缺项。"""

import json
import sys
import tomllib
import xml.etree.ElementTree as ET
from pathlib import Path


# 草案仅消费真实 JUnit testcase，不把声明的预期写成执行结果。
def verify(xml, mapping):
    rows = ET.parse(xml).getroot().findall(".//testcase")
    if not rows:
        raise ValueError("empty JUnit")
    found = {}
    for row in rows:
        identity = row.get("classname", "") + "::" + row.get("name", "")
        if identity in found:
            raise ValueError(f"duplicate JUnit identity: {identity}")
        if any(row.find(tag) is not None for tag in ("failure", "error", "skipped")):
            raise ValueError(f"non-passing JUnit testcase: {identity}")
        found[identity] = row
    acs = tomllib.loads(mapping.read_text())["acceptance"]
    if {row["id"] for row in acs} != {f"AC-{i}" for i in range(1, 7)}:
        raise ValueError("acceptance draft must cover exactly AC-1..6")
    required = set()
    for ac in acs:
        for field in ("id", "kind", "entry", "observe", "tests", "doubles", "mutation"):
            if not ac.get(field):
                raise ValueError(f"missing {ac['id']}.{field}")
        for identity in ac["tests"]:
            required.add(identity)
            if identity not in found:
                raise ValueError(f"missing actual JUnit testcase: {identity}")
    if required != found.keys():
        raise ValueError(f"unmapped testcase: {sorted(found.keys() - required)}")
    return {"actual_testcases": len(found), "acceptance_ids": [ac["id"] for ac in acs]}


# 此入口只在 pytest 成功后执行；异常保持非零退出。
if __name__ == "__main__":
    result = verify(Path(sys.argv[1]), Path(sys.argv[2]))
    Path(sys.argv[3]).write_text(json.dumps(result, indent=2))
