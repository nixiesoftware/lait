import React from "react";

interface BreadcrumbProps {
    pageTitle: string,
    breadcrumbItems?: [{ label: string; href: string }, { label: string }]
}

const PageBreadCrumb: React.FC<BreadcrumbProps> = ({pageTitle}) => {
    return (
        <div className="flex flex-wrap items-center justify-between gap-3">
            <h2
                className="text-xl font-semibold text-gray-800 dark:text-white/90"
                x-text="pageName"
            >
                {pageTitle}
            </h2>
        </div>
    );
};

export default PageBreadCrumb;
