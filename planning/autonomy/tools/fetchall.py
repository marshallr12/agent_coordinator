"""Paginate every read-only collection and per-task history into svc/."""
import json, os, urllib.parse
from get import get
P = "/api/v1/projects/fe95a6c5-2aad-463f-8446-4366d9a281c7"
KINDS = ["attempts","checkpoints","checkouts","jobs","job_observations","resources","artifacts","submissions","reviews","integrations","task_revisions","events"]

def paged(path, params=None):
    """Follow next_cursor until exhausted; return list of page data dicts."""
    params = dict(params or {}); pages = []
    while True:
        st, body = get(path + "?" + urllib.parse.urlencode(params))
        if st != 200: pages.append({"_status": st, "_body": body}); break
        d = body["data"]; pages.append(d)
        if not d.get("next_cursor"): break
        params["cursor"] = d["next_cursor"]
    return pages

def items(pages): return [i for p in pages for i in p.get("items", [])]

json.dump(items(paged(P+"/events", {"limit":200})), open("svc/events_all.json","w"), indent=1)
json.dump(paged(P+"/exports", {"limit":500}), open("svc/export_pages.json","w"), indent=1)
tasks = items(paged(P+"/tasks")) + items(paged(P+"/tasks/archived"))
json.dump(tasks, open("svc/all_tasks.json","w"), indent=1)
os.makedirs("svc/hist", exist_ok=True)
for t in tasks:
    h = {k: paged(f"{P}/tasks/{t['id']}/history", {"kind":k,"limit":200}) for k in KINDS}
    st, detail = get(f"{P}/tasks/{t['id']}")
    h["_detail"] = detail
    json.dump(h, open(f"svc/hist/{t['id']}.json","w"), indent=1)
print(len(tasks), "tasks")
