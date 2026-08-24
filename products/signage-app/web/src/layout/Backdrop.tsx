import { useSidebar } from "@/context/SidebarContext";
import React from "react";

const Backdrop: React.FC = () => {
  const { isMobileOpen, toggleMobileSidebar } = useSidebar();
  if (!isMobileOpen) return null;
  return (
    <div className="ds-backdrop" style={{ zIndex: 40 }} onClick={toggleMobileSidebar} />
  );
};

export default Backdrop;
