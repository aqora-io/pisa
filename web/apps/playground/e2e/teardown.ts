// Deletes what the walk created: every snapshot labelled e2e=<run>, with
// what replaces it.
export default async function teardown() {
  const url = process.env["PISA_URL"];
  const run = process.env["PISA_E2E_RUN"];
  if (url === undefined || run === undefined) return;
  const listed = await fetch(`${url}/snapshots?label=e2e:${run}&limit=1000`);
  if (!listed.ok) return;
  const { snapshots } = (await listed.json()) as { snapshots: { id: string }[] };
  for (const { id } of snapshots) {
    await fetch(`${url}/snapshots/${id}?with_replacers=true`, { method: "DELETE" });
  }
}
