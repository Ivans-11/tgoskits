use alloc::{string::String, vec::Vec};

use ax_errno::{AxResult, ax_err_type};
use fdt_parser::Node;

use super::{
    create::{cpu_node_id, fdt_write_err, should_skip_guest_cpu_prop},
    vm_fdt::{FdtWriter, FdtWriterNode},
};

/// Rewrites the AArch64 CPU topology according to the VM configuration.
///
/// The host FDT supplies an architectural CPU-node template, while
/// `phys_cpu_ids` defines the CPUs visible to the guest. This lets a guest
/// expose more vCPUs than the host without making the guest topology depend on
/// the number of host CPU nodes.
pub(super) struct Aarch64FdtRewriter<'a> {
    cpu_template: Option<Node<'a>>,
    copied_cpu_ids: Vec<usize>,
    added_missing_cpus: bool,
    previous_node_path: String,
}

impl<'a> Aarch64FdtRewriter<'a> {
    pub(super) fn new(nodes: &[Node<'a>]) -> Self {
        let cpu_template = nodes
            .iter()
            .find(|node| node.name().starts_with("cpu@"))
            .cloned();

        Self {
            cpu_template,
            copied_cpu_ids: Vec::new(),
            added_missing_cpus: false,
            previous_node_path: String::new(),
        }
    }

    pub(super) fn before_node(
        &mut self,
        node_path: &str,
        phys_cpu_ids: &[usize],
        previous_node_level: &mut usize,
        node_stack: &mut Vec<FdtWriterNode>,
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        if self.previous_node_path.starts_with("/cpus")
            && !node_path.starts_with("/cpus")
            && !self.added_missing_cpus
        {
            self.close_cpu_children(previous_node_level, node_stack, fdt_writer)?;
            self.add_missing_cpu_nodes(phys_cpu_ids, fdt_writer)?;
        }
        Ok(())
    }

    pub(super) fn observe_cpu_node(&mut self, _node: Node<'a>, node_path: &str) {
        if let Some(cpu_id) = cpu_node_id(node_path)
            && !self.copied_cpu_ids.contains(&cpu_id)
        {
            self.copied_cpu_ids.push(cpu_id);
        }
    }

    pub(super) fn write_rewritten_property(
        &self,
        _node: &Node,
        _node_path: &str,
        _prop_name: &str,
        _prop_value: &[u8],
        _phys_cpu_ids: &[usize],
        _fdt_writer: &mut FdtWriter,
    ) -> AxResult<bool> {
        Ok(false)
    }

    pub(super) fn after_node(&mut self, node_path: &str) {
        self.previous_node_path.clear();
        self.previous_node_path.push_str(node_path);
    }

    pub(super) fn finish(
        &mut self,
        phys_cpu_ids: &[usize],
        previous_node_level: &mut usize,
        node_stack: &mut Vec<FdtWriterNode>,
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        if self.previous_node_path.starts_with("/cpus") && !self.added_missing_cpus {
            self.close_cpu_children(previous_node_level, node_stack, fdt_writer)?;
            self.add_missing_cpu_nodes(phys_cpu_ids, fdt_writer)?;
        }
        Ok(())
    }

    fn close_cpu_children(
        &self,
        previous_node_level: &mut usize,
        node_stack: &mut Vec<FdtWriterNode>,
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        while *previous_node_level > 2 {
            let node = node_stack
                .pop()
                .ok_or_else(|| ax_err_type!(InvalidData, "Guest FDT CPU node stack is empty"))?;
            fdt_writer.end_node(node).map_err(fdt_write_err)?;
            *previous_node_level -= 1;
        }
        Ok(())
    }

    fn add_missing_cpu_nodes(
        &mut self,
        phys_cpu_ids: &[usize],
        fdt_writer: &mut FdtWriter,
    ) -> AxResult {
        self.added_missing_cpus = true;

        let cpu_template = self.cpu_template.as_ref().ok_or_else(|| {
            ax_err_type!(InvalidData, "Host FDT does not contain an AArch64 CPU node")
        })?;

        for &cpu_id in phys_cpu_ids {
            if self.copied_cpu_ids.contains(&cpu_id) {
                continue;
            }

            let cpu = fdt_writer
                .begin_node(&alloc::format!("cpu@{cpu_id:x}"))
                .map_err(fdt_write_err)?;
            for prop in cpu_template.propertys() {
                if should_skip_guest_cpu_prop(prop.name) {
                    continue;
                }
                if prop.name == "reg" {
                    write_cpu_reg_property(fdt_writer, prop.raw_value(), cpu_id)?;
                } else if !matches!(prop.name, "phandle" | "linux,phandle") {
                    fdt_writer
                        .property(prop.name, prop.raw_value())
                        .map_err(fdt_write_err)?;
                }
            }
            fdt_writer.end_node(cpu).map_err(fdt_write_err)?;
            self.copied_cpu_ids.push(cpu_id);
        }

        Ok(())
    }
}

fn write_cpu_reg_property(
    fdt_writer: &mut FdtWriter,
    template_value: &[u8],
    cpu_id: usize,
) -> AxResult {
    match template_value.len() {
        4 => fdt_writer
            .property_u32("reg", cpu_id as u32)
            .map_err(fdt_write_err),
        8 => fdt_writer
            .property_u64("reg", cpu_id as u64)
            .map_err(fdt_write_err),
        _ => Err(ax_err_type!(
            InvalidData,
            "AArch64 CPU reg property must contain one or two cells"
        )),
    }
}
