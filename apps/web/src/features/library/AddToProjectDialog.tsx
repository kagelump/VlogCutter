import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { keys, useClient } from "../../app/client";
import { plural } from "../../lib/format";
import { Button, Checkbox, Dialog, ErrorMessage, Field, Loading, Notice, Stats } from "../../ui";
import { projectName } from "../projects/projectName";

export function AddToProjectDialog({ open, mediaObjectIds, onClose, onAdded }: {
  open: boolean;
  mediaObjectIds: string[];
  onClose: () => void;
  onAdded: () => void;
}) {
  const client = useClient();
  const queryClient = useQueryClient();
  const projects = useQuery({ queryKey: keys.projects, queryFn: () => client.listProjects(100), enabled: open });
  const [projectId, setProjectId] = useState("");
  const [acknowledge, setAcknowledge] = useState(false);
  const selectionKey = mediaObjectIds.join(",");
  const request = useMemo(() => ({
    mediaObjectIds,
    acknowledgeUnverified: acknowledge,
    idempotencyKey: `web_${selectionKey.replaceAll(",", "_")}`.slice(0, 250),
  }), [acknowledge, mediaObjectIds, selectionKey]);

  useEffect(() => {
    if (!projectId && projects.data?.items[0]) setProjectId(projects.data.items[0].projectId);
  }, [projectId, projects.data]);
  useEffect(() => { if (!open) setAcknowledge(false); }, [open]);

  const preview = useQuery({
    queryKey: ["add-preview", projectId, selectionKey, acknowledge],
    queryFn: () => client.previewAddToProject(projectId, request),
    enabled: open && Boolean(projectId) && mediaObjectIds.length > 0,
  });

  const add = useMutation({
    mutationFn: () => client.addToProject(projectId, request),
    onSuccess: (result) => {
      void queryClient.invalidateQueries({ queryKey: keys.project(projectId) });
      void queryClient.invalidateQueries({ queryKey: keys.projects });
      if (result.blockedCount === 0) onAdded(); else void preview.refetch();
    },
  });

  const data = preview.data;
  const blocked = Boolean(data?.mismatchCount) || (Boolean(data?.acknowledgementRequiredCount) && !acknowledge);

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Add ${plural(mediaObjectIds.length, "file")} to a project`}
      footer={<>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="primary" busy={add.isPending} disabled={!projectId || !data || blocked} onClick={() => add.mutate()}>Add</Button>
      </>}
    >
      <Field label="Project">
        {(id) => (
          <select id={id} value={projectId} onChange={(event) => setProjectId(event.target.value)}>
            {projects.data?.items.map((project) => <option key={project.projectId} value={project.projectId}>{projectName(project.projectId)}</option>)}
          </select>
        )}
      </Field>
      {preview.isFetching && !data ? <Loading label="Checking…" /> : null}
      {data ? (
        <Stats label="What will happen" items={[
          { label: "Will be added", value: data.readyCount },
          { label: "Already in project", value: data.alreadyMemberCount },
          data.acknowledgementRequiredCount ? { label: "Unverified", value: data.acknowledgementRequiredCount } : null,
          data.mismatchCount ? { label: "Blocked", value: data.mismatchCount } : null,
        ]} />
      ) : null}
      {data?.mismatchCount ? <Notice tone="critical">{plural(data.mismatchCount, "file")} failed identity checks. Deselect them to continue.</Notice> : null}
      {data?.acknowledgementRequiredCount ? (
        <Checkbox checked={acknowledge} onChange={setAcknowledge}>
          Add the {plural(data.acknowledgementRequiredCount, "unverified file")} anyway. Their identity state is recorded with them.
        </Checkbox>
      ) : null}
      <ErrorMessage error={projects.error ?? preview.error ?? add.error} />
    </Dialog>
  );
}
