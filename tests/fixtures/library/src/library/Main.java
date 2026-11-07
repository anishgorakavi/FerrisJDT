package library;

import java.util.List;

public class Main {
    public static void main(String[] args) {
        Library library = new Library();

        library.addBook(new Book("Effective Java", "Joshua Bloch", "978-0134685991", Genre.NONFICTION));
        library.addBook(new Book("Dune", "Frank Herbert", "978-0441013593", Genre.FICTION));
        library.addBook(new Book("A Brief History of Time", "Stephen Hawking", "978-0553380163", Genre.SCIENCE));

        Student alice = new Student("S100", "Alice", "Computer Science");
        Faculty drKim = new Faculty("F200", "Dr. Kim", "Physics");
        library.registerMember(alice);
        library.registerMember(drKim);

        List<Book> books = library.books();
        Book book = books.get(0);
        library.checkout(book, alice, 10);

        for (Book sorted : library.sortedByTitle()) {
            System.out.println(sorted.getTitle());
        }

        int today = 15;
        for (Checkout overdue : library.overdueCheckouts(today)) {
            double fine = LibraryUtils.fineFor(overdue, today, 25.0);
            System.out.println(overdue.getBook().getTitle() + ": $" + fine);
        }

        library.returnBook(book);

        if (book.getGenre() == Genre.FICTION) {
            System.out.println("fiction");
        } else if (book.getGenre() == Genre.NONFICTION) {
            System.out.println("nonfiction");
        } else {
            System.out.println("other");
        }
    }
}
