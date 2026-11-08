import { useInfiniteQuery, useMutation, useQuery, useQueryClient, type InfiniteData } from "@tanstack/react-query";
import type { ProjectAssetItem, ProjectAssetPage, ProjectReviewAsset, ReviewMutationResult } from "@rcdvlog/project-client";
import { keys, useClient } from "../../app/client";
import { useLiveEvents } from "../../lib/hooks";

const PAGE_SIZE = 100;
/** Poll this often when the change feed is down so other tabs' edits still arrive. */
const FALLBACK_POLL_MS = 5_000;
const LIVE_POLL_MS = 60_000;

export function useClipQueue(projectId: string, search: string) {
  const client = useClient();
  const live = useLiveEvents(projectId, keys.project(projectId));
  const query = useInfiniteQuery({
    queryKey: keys.assets(projectId, search),
    queryFn: ({ pageParam }) => client.listProjectAssets(projectId, PAGE_SIZE, pageParam ?? undefined, search || undefined),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.nextCursor,
    refetchInterval: live ? LIVE_POLL_MS : FALLBACK_POLL_MS,
    placeholderData: (previous) => previous,
  });
  const assets: ProjectAssetItem[] = query.data?.pages.flatMap((page) => page.items) ?? [];
  return { ...query, assets, live };
}

export function useReviewAsset(projectId: string, assetId: string | null, live: boolean) {
  const client = useClient();
  return useQuery({
    queryKey: keys.asset(projectId, assetId ?? ""),
    queryFn: () => client.getProjectReviewAsset(projectId, assetId!),
    enabled: Boolean(assetId),
    refetchInterval: live ? LIVE_POLL_MS : FALLBACK_POLL_MS,
  });
}

interface AssetEdit {
  asset: ProjectReviewAsset;
  optimistic: (asset: ProjectReviewAsset) => ProjectReviewAsset;
  command: (baseRevision: number) => Promise<ReviewMutationResult>;
}

/**
 * Review edits are revisioned server-side. Each edit is applied optimistically
 * to both the clip and the queue, then replaced by the server's materialized
 * asset (or rolled back on failure).
 */
export function useAssetEdit(projectId: string) {
  const queryClient = useQueryClient();

  function write(asset: ProjectReviewAsset) {
    queryClient.setQueryData(keys.asset(projectId, asset.projectAssetId), asset);
    queryClient.setQueriesData<InfiniteData<ProjectAssetPage>>({ queryKey: ["project", projectId, "assets"] }, (data) => data && {
      ...data,
      pages: data.pages.map((page) => ({
        ...page,
        items: page.items.map((item) => item.projectAssetId === asset.projectAssetId ? { ...item, ...pickListFields(asset) } : item),
      })),
    });
  }

  return useMutation({
    mutationFn: ({ asset, command }: AssetEdit) => command(asset.materializedRevision),
    onMutate: async ({ asset, optimistic }) => {
      await queryClient.cancelQueries({ queryKey: keys.asset(projectId, asset.projectAssetId) });
      write(optimistic(asset));
      return { before: asset };
    },
    onError: (_error, _edit, context) => { if (context) write(context.before); },
    onSuccess: (result) => write(result.asset),
  });
}

function pickListFields(asset: ProjectReviewAsset): ProjectAssetItem {
  const { projectAssetId, filename, camera, mediaObjectId, mediaVerificationState, removed, rotationDegrees, labels, activeNote, durationSeconds, bestTime } = asset;
  return { projectAssetId, filename, camera, mediaObjectId, mediaVerificationState, removed, rotationDegrees, labels, activeNote, durationSeconds, bestTime };
}
