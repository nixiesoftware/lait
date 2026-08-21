import React, { useState, useMemo } from "react";
import { Search, Plus, GalleryThumbnailsIcon } from "lucide-react";
import { mediaChip, mediaKind } from "@/components/broadcasts/types";
import type { SignageMedia } from "@/utils/lait/types";
import { uploadContent } from "@/utils/content/api";
import { isImageContent, isVideoContent } from "@/utils/uploads/contentTypeUtils";
import { MdVideoFile } from "react-icons/md";
import { MdImage } from "@react-icons/all-files/md/MdImage";
import { AddContentButton } from "@/components/content";
import { HiMiniPhoto } from "react-icons/hi2";
import { AiFillVideoCamera } from "@react-icons/all-files/ai/AiFillVideoCamera";

interface ContentLibraryProps {
  allContent: SignageMedia[];
  onAddContent: (media: SignageMedia) => void;
  /** Optional callback used by hosts (e.g. modals) to close after selecting content. */
  onRequestClose?: () => void;
  onContentUploaded?: () => void;
}

interface PendingUpload {
  key: string;
  name: string;
  mime: string;
}

export default function ContentLibrary({
  allContent,
  onAddContent,
  onRequestClose,
  onContentUploaded,
}: ContentLibraryProps) {
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedType, setSelectedType] = useState<string>("all");
  const [pendingUploads, setPendingUploads] = useState<PendingUpload[]>([]);

  const handleSelectContent = (media: SignageMedia) => {
    onAddContent(media);
    onRequestClose?.();
  };

  const handleFilesSelected = async (files: File[]) => {
    if (files.length === 0) return;

    const pending: PendingUpload[] = files.map((file, index) => ({
      key: `upload-${Date.now()}-${index}`,
      name: file.name.replace(/\.[^/.]+$/, ""),
      mime: file.type,
    }));
    setPendingUploads(prev => [...prev, ...pending]);

    try {
      await uploadContent(files);
      onContentUploaded?.();
    } catch (e) {
      console.error('Upload failed:', e);
    } finally {
      const keys = new Set(pending.map(p => p.key));
      setPendingUploads(prev => prev.filter(p => !keys.has(p.key)));
    }
  };

  // Filter the library based on search and type. Integration entries are
  // added from the Apps panel, not from here.
  const filteredContent = useMemo(() => {
    const filtered = allContent.filter(media => {
      if (media.source === 'kind') return false;
      const kind = mediaKind(media);
      const matchesSearch = media.name.toLowerCase().includes(searchQuery.toLowerCase());
      const matchesType = selectedType === "all" ||
        (selectedType === "image/" && isImageContent(kind)) ||
        (selectedType === "video/" && isVideoContent(kind));
      return matchesSearch && matchesType;
    });

    return filtered.sort((a, b) => a.name.localeCompare(b.name));
  }, [allContent, searchQuery, selectedType]);

  const renderPendingItem = (pending: PendingUpload) => (
    <div key={pending.key} className="rounded-xs w-full">
      <div className="relative aspect-[9/8] overflow-hidden rounded-xs px-4 py-3 flex items-center justify-center
        bg-gray-100 dark:bg-gray-800 animate-pulse">
        <div className="flex flex-col items-center justify-center text-center">
          <p className="text-sm font-medium text-gray-700 dark:text-gray-300 truncate max-w-full">{pending.name}</p>
          <p className="mt-2 text-xs text-gray-500 dark:text-gray-400">Uploading…</p>
        </div>
      </div>
    </div>
  );

  const renderGridItem = (media: SignageMedia) => {
    const kind = mediaKind(media);
    return (
      <div
        key={media.id}
        className="rounded-xs cursor-pointer group w-full"
        onClick={(e) => {
          e.stopPropagation();
          handleSelectContent(media);
        }}
      >
        <div className="relative aspect-[9/8] overflow-hidden rounded-xs px-4 py-3 flex items-center justify-center
          bg-gray-100 dark:bg-gray-800 group-hover:bg-gray-200 dark:group-hover:bg-gray-700 transition-colors">
          {/* Add button overlay */}
          <div className="absolute inset-0 flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity bg-black/20 z-10">
            <button className="p-2 bg-white/90 dark:bg-gray-800/90 rounded-full shadow-lg hover:scale-110 transition-transform">
              <Plus className="w-6 h-6 text-gray-700 dark:text-gray-300" />
            </button>
          </div>

          {/* Placeholder tile: stored bytes have no browser URL yet */}
          <div className="w-full h-full flex flex-col items-center justify-center gap-2 text-center">
            {isImageContent(kind) ? (
              <MdImage className="w-10 h-10 text-gray-400" />
            ) : isVideoContent(kind) ? (
              <MdVideoFile className="w-10 h-10 text-gray-400" />
            ) : (
              <GalleryThumbnailsIcon className="w-10 h-10 text-gray-400" />
            )}
            <p className="text-sm font-medium text-gray-700 dark:text-gray-300 truncate max-w-full px-1">
              {media.name}
            </p>
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-gray-200 dark:bg-gray-700 text-gray-600 dark:text-gray-300">
              {mediaChip(media)}
            </span>
          </div>
        </div>

        <div className="py-2">
          <p className="text-md font-medium text-left truncate dark:text-white">
            {media.name}
          </p>
          <p className="text-xs font-normal text-left truncate flex justify-between text-gray-500 dark:text-white">
            <span className="flex items-center gap-1">
              {isImageContent(kind) ? (
                <MdImage className="w-3 h-3" />
              ) : (
                <MdVideoFile className="w-3 h-3" />
              )}
              {mediaChip(media)}
            </span>
            <span>
              {media.width && media.height ? `${media.width}x${media.height}` : ""}
            </span>
          </p>
        </div>
      </div>
    );
  };

  return (
    <div className="h-full flex flex-row">

      {/* Header */}
      <div className="px-2 py-4 fixed text-left max-sm:hidden">
        {/* Filters */}
        <div className="flex flex-col items-center gap-1 mb-2 min-w-[200px]">
          <p className="text-xs w-full text-left text-gray-600 px-1">Library</p>
          <button className={`w-full flex flex-row gap-1 items-center py-1 px-1 rounded-md text-left ${
            selectedType === "all" ? 'bg-gray-200' : 'bg-white'
          }`}
                  onClick={(e) => {
                    setSelectedType("all");
                    e.preventDefault();
                    e.stopPropagation();
                  }
                  }>
            <GalleryThumbnailsIcon className="text-white w-5 h-5 bg-red-700 p-0.5 rounded-xs" />
            All Media
          </button>
          <button className={`w-full flex flex-row gap-1 items-center py-1 px-1 rounded-md text-left ${
            selectedType === "image/" ? 'bg-gray-200' : 'bg-white'
          }`}
            onClick={(e) => {
              setSelectedType("image/");
              e.preventDefault();
              e.stopPropagation();
            }
          }>
            <HiMiniPhoto className="text-white w-5 h-5 bg-green-700 p-0.5 rounded-xs" />
            Images
          </button>
          <button className={`w-full flex flex-row gap-1 items-center py-1 px-1 rounded-md text-left ${
            selectedType === "video/" ? 'bg-gray-200' : 'bg-white'
          }`}
                  onClick={(e) => {
                    setSelectedType("video/");
                    e.preventDefault();
                    e.stopPropagation();
                  }
                  }>
            <AiFillVideoCamera className="text-white w-5 h-5 bg-brand-700/80 p-0.5 rounded-xs" />
            Videos
          </button>
        </div>

        {/* Upload Button */}
        <div
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
        >
          <AddContentButton
            onFilesSelected={handleFilesSelected}
            onAdd={onContentUploaded}
            className="w-full h-8"
          />
        </div>
      </div>

      {/* Content List */}
      <div
        className="flex-1 pt-1 p-4 min-md:ml-52"
        style={{ WebkitOverflowScrolling: 'touch' }}
        onMouseDown={(e) => e.stopPropagation()}
        onClick={(e) => e.stopPropagation()}
        onWheel={(e) => e.stopPropagation()}
      >
        {/* Search */}
        <div className="relative mb-2">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400" />
          <input
            type="text"
            placeholder="Search content..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onClick={(e) => {
              e.stopPropagation()
              e.preventDefault();
            }}
            className="w-full pl-9 pr-3 py-1.5 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-sm text-sm focus:outline-none focus:ring-2 focus:ring-brand-500"
          />
        </div>

        {/* Upload Button (Mobile) */}
        <div
          onMouseDown={(e) => e.stopPropagation()}
          onClick={(e) => e.stopPropagation()}
          onPointerDown={(e) => e.stopPropagation()}
        >
          <AddContentButton
            onFilesSelected={handleFilesSelected}
            onAdd={onContentUploaded}
            className="hidden max-sm:flex w-full h-10"
          />
        </div>
        {filteredContent.length === 0 && pendingUploads.length === 0 ? (
          <div className="text-center py-8">
            <p className="text-gray-500 dark:text-gray-400">No content found</p>
          </div>
        ) : (
          <div className="grid grid-cols-2 gap-3 py-6">
            {pendingUploads.map(renderPendingItem)}
            {filteredContent.map(renderGridItem)}
          </div>
        )}
      </div>
    </div>
  );
}
