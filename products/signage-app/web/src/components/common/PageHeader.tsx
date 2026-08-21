import React from "react";

interface PageHeaderProps {
  pageTitle: string;
  children?: React.ReactNode;
}

const PageHeader: React.FC<PageHeaderProps> = ({ pageTitle, children }) => {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 sm:gap-4 pt-4 pb-2">
      <h1 className="text-xl font-semibold text-gray-800 dark:text-white/90 shrink-0">
        {pageTitle}
      </h1>
      {children && (
        <div className="flex items-center gap-2">{children}</div>
      )}
    </div>
  );
};

export default PageHeader;
