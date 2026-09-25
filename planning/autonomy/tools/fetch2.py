"""Fetch full export and knowledge details (read-only)."""
import json, urllib.parse
from get import get
P = "/api/v1/projects/fe95a6c5-2aad-463f-8446-4366d9a281c7"
def paged(path, params):
    pages=[]; params=dict(params)
    while True:
        st, b = get(path+"?"+urllib.parse.urlencode(params))
        if st!=200: pages.append({"_status":st,"_body":b}); break
        pages.append(b["data"])
        if not b["data"].get("next_cursor"): break
        params["cursor"]=b["data"]["next_cursor"]
    return pages
ex = paged(P+"/exports", {"limit":200})
json.dump(ex, open("svc/export_pages.json","w"), indent=1)
open("svc/export.md","w").write("\n".join(p.get("markdown","") for p in ex))
k = json.load(open("svc/knowledge.json"))["data"]["items"]
det = {}
for i in k:
    st,b = get(f"{P}/knowledge/{i['id']}"); det[i["id"]] = b
json.dump(det, open("svc/knowledge_detail.json","w"), indent=1)
print(len(ex), [p.get("page_complete") for p in ex][-3:], len(k))
