/**
 * Astrolabe's glyphs, drawn from Tabler Icons and pinned here to one size and
 * weight so every surface reaches the same set the same way. Tabler draws on a
 * 24-grid; stroke 1.8 there renders 1.5px at our 20px default — the exact
 * weight of the hand-drawn set this file replaces — and keeps the same
 * stroke-to-size ratio at every other size. `currentColor` throughout — an
 * icon is text-colored wherever it sits.
 */
import {
  IconAddressBook as TbAddressBook,
  IconAlertTriangle as TbAlertTriangle,
  IconArrowLeft as TbArrowLeft,
  IconBroadcast as TbBroadcast,
  IconCheck as TbCheck,
  IconCircleCheck as TbCircleCheck,
  IconCopy as TbCopy,
  IconDeviceLaptop as TbDeviceLaptop,
  IconDevices as TbDevices,
  IconDeviceTv as TbDeviceTv,
  IconDots as TbDots,
  IconInbox as TbInbox,
  IconPlanet as TbPlanet,
  IconPlus as TbPlus,
  IconRefresh as TbRefresh,
  IconRefreshDot as TbRefreshDot,
  IconSearch as TbSearch,
  IconServer as TbServer,
  IconUser as TbUser,
  IconUserPlus as TbUserPlus,
  IconX as TbX,
  type TablerIcon,
} from "@tabler/icons-react";

function pinned(Glyph: TablerIcon) {
  return function Icon({ size = 20 }: { size?: number }) {
    return <Glyph size={size} stroke={1.8} aria-hidden focusable={false} />;
  };
}

export const IconArrowLeft = pinned(TbArrowLeft);
export const IconDismiss = pinned(TbX);
export const IconCopy = pinned(TbCopy);
export const IconMore = pinned(TbDots);
export const IconTv = pinned(TbDeviceTv);
export const IconLaptop = pinned(TbDeviceLaptop);
export const IconCheckCircle = pinned(TbCircleCheck);
export const IconAdd = pinned(TbPlus);

// The operational bar's vocabulary: what the identity is, what a count counts,
// where a button goes.
export const IconIdentity = pinned(TbBroadcast);
export const IconAttention = pinned(TbAlertTriangle);
export const IconRestart = pinned(TbRefresh);
export const IconRollForward = pinned(TbRefreshDot);
export const IconHead = pinned(TbServer);
export const IconSpace = pinned(TbPlanet);
export const IconBook = pinned(TbAddressBook);
export const IconDevices = pinned(TbDevices);

// The address book's vocabulary.
export const IconSearch = pinned(TbSearch);
export const IconCheck = pinned(TbCheck);
export const IconUser = pinned(TbUser);
export const IconUserPlus = pinned(TbUserPlus);
export const IconIncoming = pinned(TbInbox);
