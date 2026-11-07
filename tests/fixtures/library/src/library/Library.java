package library;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;

public class Library {
    private final List<Book> books = new ArrayList<Book>();
    private final List<Member> members = new ArrayList<Member>();
    private final List<Checkout> checkouts = new ArrayList<Checkout>();

    public void addBook(Book book) {
        books.add(book);
    }

    public void registerMember(Member member) {
        members.add(member);
    }

    public Member findMember(String id) {
        for (Member member : members) {
            if (member.getId().equals(id)) {
                return member;
            }
        }
        return null;
    }

    public boolean checkout(Book book, Member member, int dueDay) {
        if (book.isCheckedOut()) {
            return false;
        }
        int active = 0;
        for (Checkout checkout : checkouts) {
            if (checkout.getMember() == member) {
                active++;
            }
        }
        if (active >= member.maxCheckouts()) {
            return false;
        }
        book.setCheckedOut(true);
        checkouts.add(new Checkout(book, member, dueDay));
        return true;
    }

    public void returnBook(Book book) {
        book.setCheckedOut(false);
        checkouts.removeIf(checkout -> checkout.getBook() == book);
    }

    public List<Checkout> overdueCheckouts(int today) {
        List<Checkout> overdue = new ArrayList<Checkout>();
        for (Checkout checkout : checkouts) {
            if (checkout.isOverdue(today)) {
                overdue.add(checkout);
            }
        }
        return overdue;
    }

    public List<Book> sortedByTitle() {
        List<Book> sorted = new ArrayList<Book>(books);
        sorted.sort(Comparator.comparing(Book::getTitle));
        return sorted;
    }

    public List<Book> books() {
        return books;
    }
}
